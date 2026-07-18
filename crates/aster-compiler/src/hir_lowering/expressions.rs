use super::types::{
    binary_type, conditional_type, constant_expression, convert, literal_value, promoted,
};
use super::{Lowerer, ast, hir};

impl Lowerer<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn expression(&mut self, expression: &ast::Expression) -> hir::Expression {
        let model_key = crate::semantic::ModelNodeKey {
            context: self.model_context.clone(),
            span: expression.span,
        };
        if let Some(resolved) = self.model.enum_values.get(&model_key).cloned() {
            let enum_symbol = self.types[&resolved.enum_name];
            let (case_symbol, field_symbols) =
                self.enum_cases[&(resolved.enum_name, resolved.case_index)].clone();
            let arguments: &[ast::Expression] = match &expression.kind {
                ast::ExpressionKind::Call { arguments, .. } => arguments,
                ast::ExpressionKind::Member { .. } => &[],
                _ => unreachable!("enum case metadata belongs to a case expression"),
            };
            let fields = arguments
                .iter()
                .zip(field_symbols)
                .map(|(argument, field)| {
                    let target = self.symbol_types[&field].clone();
                    hir::FieldValue {
                        field,
                        value: convert(self.expression(argument), &target),
                    }
                })
                .collect();
            return hir::Expression {
                type_: hir::Type::Enum(enum_symbol),
                kind: hir::ExpressionKind::EnumValue {
                    enum_symbol,
                    case: case_symbol,
                    tag: u32::try_from(resolved.case_index).expect("validated enum tag"),
                    fields,
                },
            };
        }
        match &expression.kind {
            ast::ExpressionKind::Literal(literal) => {
                let (literal, type_) = literal_value(literal);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Literal(literal),
                }
            }
            ast::ExpressionKind::Name(name) => {
                if let Some(key) = self.model.property_reads.get(&model_key) {
                    let getter = self.callable_symbols[key];
                    let receiver = self
                        .current_receiver
                        .expect("validated property read has a receiver");
                    return self.property_get(
                        hir::Expression {
                            type_: self.symbol_types[&receiver].clone(),
                            kind: hir::ExpressionKind::Symbol(receiver),
                        },
                        getter,
                    );
                }
                let symbol = self
                    .lookup(name)
                    .expect("validated names resolve to a declaration");
                if let Some(value) = self.constant_values.get(&symbol) {
                    return constant_expression(value);
                }
                if self.member_owners.contains_key(&symbol)
                    && let Some(receiver) = self.current_receiver
                {
                    let object = hir::Expression {
                        type_: self.symbol_types[&receiver].clone(),
                        kind: hir::ExpressionKind::Symbol(receiver),
                    };
                    let type_ = self
                        .symbol_types
                        .get(&symbol)
                        .cloned()
                        .or_else(|| self.callable_results.get(&symbol).cloned())
                        .unwrap_or(hir::Type::Unknown);
                    return hir::Expression {
                        type_,
                        kind: hir::ExpressionKind::Member {
                            object: Box::new(object),
                            symbol,
                        },
                    };
                }
                let type_ = self
                    .symbol_types
                    .get(&symbol)
                    .cloned()
                    .unwrap_or(hir::Type::Unknown);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Symbol(symbol),
                }
            }
            ast::ExpressionKind::This => {
                let receiver = self
                    .current_receiver
                    .expect("validated this has a receiver");
                hir::Expression {
                    type_: self.symbol_types[&receiver].clone(),
                    kind: hir::ExpressionKind::Symbol(receiver),
                }
            }
            ast::ExpressionKind::StructLiteral { type_name, fields } => {
                let struct_symbol = self.types[type_name];
                let fields = fields
                    .iter()
                    .map(|field| {
                        let field_symbol = self.members[&struct_symbol][&field.name];
                        let target = self.symbol_types[&field_symbol].clone();
                        let value = convert(self.expression(&field.value), &target);
                        hir::FieldValue {
                            field: field_symbol,
                            value,
                        }
                    })
                    .collect();
                hir::Expression {
                    type_: hir::Type::User(struct_symbol),
                    kind: hir::ExpressionKind::StructLiteral {
                        struct_symbol,
                        fields,
                    },
                }
            }
            ast::ExpressionKind::ArrayLiteral(elements) => {
                let mut elements = elements
                    .iter()
                    .map(|value| self.expression(value))
                    .collect::<Vec<_>>();
                let element_type = elements
                    .first()
                    .map_or(hir::Type::Unknown, |value| value.type_.clone());
                for element in &mut elements {
                    *element = convert(element.clone(), &element_type);
                }
                hir::Expression {
                    type_: hir::Type::Array(Box::new(element_type)),
                    kind: hir::ExpressionKind::ArrayLiteral(elements),
                }
            }
            ast::ExpressionKind::NewArray {
                element_type,
                length,
            } => {
                let element_type = self.resolve_type(element_type);
                hir::Expression {
                    type_: hir::Type::Array(Box::new(element_type.clone())),
                    kind: hir::ExpressionKind::NewArray {
                        element_type,
                        length: Box::new(self.expression(length)),
                    },
                }
            }
            ast::ExpressionKind::NewObject {
                type_name,
                arguments,
            } => {
                let class_symbol = self.types[type_name];
                let constructor = self.callable_symbols[&self.model.constructors[&model_key]];
                let parameter_types = self.callable_parameters[&constructor].clone();
                let arguments = arguments
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        convert(self.expression(argument), &parameter_types[index])
                    })
                    .collect();
                hir::Expression {
                    type_: hir::Type::Class(class_symbol),
                    kind: hir::ExpressionKind::NewObject {
                        class_symbol,
                        constructor,
                        arguments,
                    },
                }
            }
            ast::ExpressionKind::Index { array, index } => {
                let array = self.expression(array);
                let element_type = match &array.type_ {
                    hir::Type::Array(element) => (**element).clone(),
                    _ => hir::Type::Unknown,
                };
                hir::Expression {
                    type_: element_type,
                    kind: hir::ExpressionKind::Index {
                        array: Box::new(array),
                        index: Box::new(self.expression(index)),
                    },
                }
            }
            ast::ExpressionKind::Member { object, name } => {
                let object = self.expression(object);
                if matches!(object.type_, hir::Type::Array(_)) {
                    return hir::Expression {
                        type_: hir::Type::Int,
                        kind: hir::ExpressionKind::ArrayLength(Box::new(object)),
                    };
                }
                if object.type_ == hir::Type::String {
                    return hir::Expression {
                        type_: hir::Type::Int,
                        kind: hir::ExpressionKind::StringLength(Box::new(object)),
                    };
                }
                if let Some(key) = self.model.property_reads.get(&model_key) {
                    return self.property_get(object, self.callable_symbols[key]);
                }
                let symbol = match object.type_ {
                    hir::Type::User(owner)
                    | hir::Type::Class(owner)
                    | hir::Type::Interface(owner) => self.members[&owner][name],
                    _ => unreachable!("validated member access has a user type"),
                };
                let type_ = self
                    .symbol_types
                    .get(&symbol)
                    .cloned()
                    .unwrap_or(hir::Type::Unknown);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Member {
                        object: Box::new(object),
                        symbol,
                    },
                }
            }
            ast::ExpressionKind::Call {
                callee, arguments, ..
            } => {
                if let Some(level) = log_level(callee) {
                    let argument = arguments
                        .first()
                        .expect("validated logging calls have one argument");
                    return hir::Expression {
                        type_: hir::Type::Void,
                        kind: hir::ExpressionKind::LogCall {
                            level,
                            argument: Box::new(self.expression(argument)),
                        },
                    };
                }
                let resolved = &self.model.calls[&model_key];
                let symbol = self.callable_symbols[&resolved.callable];
                let type_ = self.callable_results[&symbol].clone();
                let parameter_types = self.callable_parameters[&symbol].clone();
                let arguments = arguments
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        let mut argument = self.expression(value);
                        if let Some(target) = parameter_types.get(index) {
                            argument = convert(argument, target);
                        }
                        argument
                    })
                    .collect();
                let callee = match resolved.dispatch {
                    crate::semantic::Dispatch::Direct => hir::Expression {
                        type_: hir::Type::Unknown,
                        kind: hir::ExpressionKind::Symbol(symbol),
                    },
                    crate::semantic::Dispatch::Instance | crate::semantic::Dispatch::Interface => {
                        let receiver = match &callee.kind {
                            ast::ExpressionKind::Member { object, .. } => self.expression(object),
                            ast::ExpressionKind::Name(_) => {
                                let receiver = self
                                    .current_receiver
                                    .expect("validated instance call has a receiver");
                                hir::Expression {
                                    type_: self.symbol_types[&receiver].clone(),
                                    kind: hir::ExpressionKind::Symbol(receiver),
                                }
                            }
                            _ => unreachable!("validated method call has a member callee"),
                        };
                        hir::Expression {
                            type_: hir::Type::Unknown,
                            kind: hir::ExpressionKind::Member {
                                object: Box::new(receiver),
                                symbol,
                            },
                        }
                    }
                };
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Call {
                        callee: Box::new(callee),
                        arguments,
                    },
                }
            }
            ast::ExpressionKind::Unary { operator, operand } => {
                if *operator == ast::UnaryOperator::Negate
                    && matches!(
                        &operand.kind,
                        ast::ExpressionKind::Literal(ast::Literal::Integer(value))
                            if value == "9223372036854775808"
                    )
                {
                    return hir::Expression {
                        type_: hir::Type::Long,
                        kind: hir::ExpressionKind::Literal(hir::Literal::Integer(
                            "-9223372036854775808".to_owned(),
                        )),
                    };
                }
                let operand = self.expression(operand);
                let type_ = operand.type_.clone();
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Unary {
                        operator: unary(*operator),
                        operand: Box::new(operand),
                    },
                }
            }
            ast::ExpressionKind::IncrementDecrement {
                operator,
                prefix,
                operand,
            } => {
                let target = self.expression(operand);
                let type_ = target.type_.clone();
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::IncrementDecrement {
                        operator: increment(*operator),
                        prefix: *prefix,
                        target: Box::new(target),
                    },
                }
            }
            ast::ExpressionKind::Try { operand } => {
                let resolved = self.model.propagations[&model_key].clone();
                let value = self.expression(operand);
                let (ok_case, ok_fields) =
                    self.enum_cases[&(resolved.result_type.clone(), resolved.ok_index)].clone();
                let (error_case, error_fields) =
                    self.enum_cases[&(resolved.result_type.clone(), resolved.error_index)].clone();
                let (return_error_case, return_error_fields) = self.enum_cases[&(
                    resolved.function_result_type.clone(),
                    resolved.function_error_index,
                )]
                    .clone();
                let ok_field = ok_fields[0];
                let error_field = error_fields[0];
                let return_error_field = return_error_fields[0];
                let success_type = self.symbol_types[&ok_field].clone();
                let error_type = self.symbol_types[&error_field].clone();
                hir::Expression {
                    type_: success_type.clone(),
                    kind: hir::ExpressionKind::PropagateResult {
                        operand: Box::new(value),
                        success_type,
                        error_type,
                        ok_case,
                        ok_field,
                        ok_tag: tag(resolved.ok_index),
                        error_case,
                        error_field,
                        error_tag: tag(resolved.error_index),
                        return_type: self.current_return.clone(),
                        return_error_case,
                        return_error_field,
                        return_error_tag: tag(resolved.function_error_index),
                    },
                }
            }
            ast::ExpressionKind::Conditional {
                condition,
                when_true,
                when_false,
            } => {
                let condition = self.expression(condition);
                let when_true = self.expression(when_true);
                let when_false = self.expression(when_false);
                let type_ = conditional_type(&when_true.type_, &when_false.type_);
                let when_true = convert(when_true, &type_);
                let when_false = convert(when_false, &type_);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Conditional {
                        condition: Box::new(condition),
                        when_true: Box::new(when_true),
                        when_false: Box::new(when_false),
                    },
                }
            }
            ast::ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                let mut left = self.expression(left);
                let mut right = self.expression(right);
                if left.type_ != right.type_
                    && let Some(promoted) = promoted(&left.type_, &right.type_)
                {
                    left = convert(left, &promoted);
                    right = convert(right, &promoted);
                }
                let type_ = binary_type(*operator, &left.type_, &right.type_);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Binary {
                        left: Box::new(left),
                        operator: binary(*operator),
                        right: Box::new(right),
                    },
                }
            }
            ast::ExpressionKind::Assignment {
                target,
                operator,
                value,
            } => {
                if let Some(resolved) = self.model.property_assignments.get(&model_key) {
                    let object = match &target.kind {
                        ast::ExpressionKind::Member { object, .. } => self.expression(object),
                        ast::ExpressionKind::Name(_) => {
                            let receiver = self
                                .current_receiver
                                .expect("validated property assignment has a receiver");
                            hir::Expression {
                                type_: self.symbol_types[&receiver].clone(),
                                kind: hir::ExpressionKind::Symbol(receiver),
                            }
                        }
                        _ => unreachable!("property target is a name or member"),
                    };
                    let setter = self.callable_symbols[&resolved.setter];
                    let target_type = self.callable_parameters[&setter][0].clone();
                    let value = convert(self.expression(value), &target_type);
                    return hir::Expression {
                        type_: target_type,
                        kind: hir::ExpressionKind::PropertyAssignment {
                            object: Box::new(object),
                            getter: resolved
                                .getter
                                .as_ref()
                                .map(|key| self.callable_symbols[key]),
                            setter,
                            operator: assignment(*operator),
                            value: Box::new(value),
                        },
                    };
                }
                let target = self.expression(target);
                let type_ = target.type_.clone();
                let value = convert(self.expression(value), &type_);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Assignment {
                        target: Box::new(target),
                        operator: assignment(*operator),
                        value: Box::new(value),
                    },
                }
            }
            ast::ExpressionKind::Cast { target, operand } => {
                let type_ = self.resolve_type(target);
                let operand = self.expression(operand);
                if operand.type_ == type_ {
                    operand
                } else {
                    hir::Expression {
                        type_,
                        kind: hir::ExpressionKind::Convert {
                            operand: Box::new(operand),
                        },
                    }
                }
            }
        }
    }

    fn property_get(&self, object: hir::Expression, getter: hir::SymbolId) -> hir::Expression {
        let type_ = self.callable_results[&getter].clone();
        hir::Expression {
            type_,
            kind: hir::ExpressionKind::Call {
                callee: Box::new(hir::Expression {
                    type_: hir::Type::Unknown,
                    kind: hir::ExpressionKind::Member {
                        object: Box::new(object),
                        symbol: getter,
                    },
                }),
                arguments: Vec::new(),
            },
        }
    }
}

/// Recognize the validated standard-library logging surface.
fn log_level(callee: &ast::Expression) -> Option<hir::LogLevel> {
    match &callee.kind {
        ast::ExpressionKind::Name(name) if name == "Log" => Some(hir::LogLevel::Log),
        ast::ExpressionKind::Member { object, name } => match &object.kind {
            ast::ExpressionKind::Name(object) if object == "Log" => match name.as_str() {
                "Warning" => Some(hir::LogLevel::Warning),
                "Error" => Some(hir::LogLevel::Error),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn tag(index: usize) -> u32 {
    u32::try_from(index).expect("validated enum tag")
}

macro_rules! convert_operator {
    ($function:ident, $source:ty, $target:ty, [$($variant:ident),+ $(,)?]) => {
        fn $function(value: $source) -> $target {
            match value { $(<$source>::$variant => <$target>::$variant,)+ }
        }
    };
}

convert_operator!(unary, ast::UnaryOperator, hir::UnaryOperator, [Not, Negate]);
convert_operator!(
    increment,
    ast::IncrementOperator,
    hir::IncrementOperator,
    [Increment, Decrement]
);
convert_operator!(
    binary,
    ast::BinaryOperator,
    hir::BinaryOperator,
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
convert_operator!(
    assignment,
    ast::AssignmentOperator,
    hir::AssignmentOperator,
    [
        Assign,
        AddAssign,
        SubtractAssign,
        MultiplyAssign,
        DivideAssign
    ]
);
