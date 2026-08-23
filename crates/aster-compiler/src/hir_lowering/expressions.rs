use super::types::{
    binary_type, conditional_type, constant_expression, convert, literal_value, promoted,
};
use super::{Lowerer, ast, hir};

impl Lowerer<'_> {
    fn binary_expression(&mut self, expression: &ast::Expression) -> hir::Expression {
        enum Work<'a> {
            Visit(&'a ast::Expression),
            Combine(ast::BinaryOperator),
        }

        let mut work = vec![Work::Visit(expression)];
        let mut values = Vec::new();
        while let Some(task) = work.pop() {
            match task {
                Work::Visit(expression) => {
                    if let ast::ExpressionKind::Binary {
                        left,
                        operator,
                        right,
                    } = &expression.kind
                    {
                        work.push(Work::Combine(*operator));
                        work.push(Work::Visit(right));
                        work.push(Work::Visit(left));
                    } else {
                        values.push(self.expression(expression));
                    }
                }
                Work::Combine(operator) => {
                    let mut right = values.pop().expect("binary right operand was lowered");
                    let mut left = values.pop().expect("binary left operand was lowered");
                    if left.type_ != right.type_
                        && let Some(promoted) = promoted(&left.type_, &right.type_)
                    {
                        left = convert(left, &promoted);
                        right = convert(right, &promoted);
                    }
                    let type_ = binary_type(operator, &left.type_, &right.type_);
                    values.push(hir::Expression {
                        type_,
                        kind: hir::ExpressionKind::Binary {
                            left: Box::new(left),
                            operator: binary(operator),
                            right: Box::new(right),
                        },
                    });
                }
            }
        }
        let result = values.pop().expect("binary expression produced a value");
        debug_assert!(values.is_empty());
        result
    }

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
            let arguments = match &expression.kind {
                ast::ExpressionKind::Call { arguments, .. } => arguments
                    .iter()
                    .map(|argument| &argument.value)
                    .collect::<Vec<_>>(),
                ast::ExpressionKind::Member { .. } => Vec::new(),
                _ => unreachable!("enum case metadata belongs to a case expression"),
            };
            let fields = arguments
                .iter()
                .zip(&resolved.argument_order)
                .map(|(argument, parameter)| {
                    let field = field_symbols[*parameter];
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
        if let ast::ExpressionKind::NewObject {
            type_name: None,
            arguments,
        } = &expression.kind
        {
            let type_name = self
                .model
                .contextual_new_types
                .get(&model_key)
                .expect("validated target-typed construction has a concrete type")
                .clone();
            let explicit = ast::Expression {
                kind: ast::ExpressionKind::NewObject {
                    type_name: Some(type_name),
                    arguments: arguments.clone(),
                },
                span: expression.span,
            };
            return self.expression(&explicit);
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
                    .map(|value| value.type_.clone())
                    .or_else(|| {
                        self.model
                            .contextual_empty_array_elements
                            .get(&model_key)
                            .map(|name| {
                                self.resolve_type(&ast::TypeRef::new(name, expression.span))
                            })
                    })
                    .unwrap_or(hir::Type::Unknown);
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
                        initialization: if self.model.zero_length_new_arrays.contains(&model_key) {
                            hir::NewArrayInitialization::Empty
                        } else {
                            hir::NewArrayInitialization::Default
                        },
                    },
                }
            }
            ast::ExpressionKind::NewObject {
                type_name: Some(type_name),
                ..
            } if type_name.starts_with("List<") => {
                // `List` is reserved and never registered in `self.types`
                // (see `validate_no_reserved_type_names`), so it is resolved
                // structurally here instead of through the class lookup
                // below. Semantic analysis already validated the constructor
                // takes no arguments, so `arguments` is guaranteed empty.
                let inner = type_name
                    .strip_prefix("List<")
                    .and_then(|rest| rest.strip_suffix('>'))
                    .expect("guarded above");
                let element_type = self.resolve_type(&ast::TypeRef::new(inner, expression.span));
                hir::Expression {
                    type_: hir::Type::List(Box::new(element_type.clone())),
                    kind: hir::ExpressionKind::NewList { element_type },
                }
            }
            ast::ExpressionKind::NewObject {
                type_name: Some(type_name),
                ..
            } if type_name.starts_with("Dictionary<") => {
                let type_ = self.resolve_type(&ast::TypeRef::new(type_name, expression.span));
                let hir::Type::Dictionary(key_type, value_type) = type_.clone() else {
                    return hir::Expression {
                        type_: hir::Type::Unknown,
                        kind: hir::ExpressionKind::Literal(hir::Literal::Integer("0".to_owned())),
                    };
                };
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::NewDictionary {
                        key_type: *key_type,
                        value_type: *value_type,
                    },
                }
            }
            ast::ExpressionKind::NewObject { .. }
                if self.model.string_builder_constructions.contains(&model_key) =>
            {
                let class_symbol = self
                    .string_builder
                    .expect("official StringBuilder symbols were predeclared")
                    .class;
                hir::Expression {
                    type_: hir::Type::Class(class_symbol),
                    kind: hir::ExpressionKind::NewStringBuilder { class_symbol },
                }
            }
            ast::ExpressionKind::NewObject {
                type_name,
                arguments,
            } => {
                let type_name = type_name
                    .as_ref()
                    .expect("target-typed construction was resolved");
                let class_symbol = self.types[type_name];
                let constructor = self.callable_symbols[&self.model.constructors[&model_key]];
                let parameter_types = self.callable_parameters[&constructor].clone();
                let plan = self.model.call_arguments[&model_key].clone();
                let mut lowered = arguments
                    .iter()
                    .zip(&plan.source_to_parameter)
                    .map(|(argument, parameter)| {
                        convert(self.expression(argument), &parameter_types[*parameter])
                    })
                    .collect::<Vec<_>>();
                let mut argument_order = plan.source_to_parameter;
                for default in plan.defaults {
                    lowered.push(convert(
                        constant_expression(&default.value),
                        &parameter_types[default.parameter],
                    ));
                    argument_order.push(default.parameter);
                }
                hir::Expression {
                    type_: hir::Type::Class(class_symbol),
                    kind: hir::ExpressionKind::NewObject {
                        class_symbol,
                        constructor,
                        arguments: lowered,
                        argument_order,
                    },
                }
            }
            ast::ExpressionKind::Index { array, index } => {
                let array = self.expression(array);
                if self.model.list_indexes.contains(&model_key) {
                    let element_type = match &array.type_ {
                        hir::Type::List(element) => (**element).clone(),
                        _ => hir::Type::Unknown,
                    };
                    return hir::Expression {
                        type_: element_type.clone(),
                        kind: hir::ExpressionKind::ListGet {
                            list: Box::new(array),
                            index: Box::new(self.expression(index)),
                            element_type,
                        },
                    };
                }
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
                if matches!(object.type_, hir::Type::List(_)) {
                    return hir::Expression {
                        type_: hir::Type::Int,
                        kind: hir::ExpressionKind::ListLength(Box::new(object)),
                    };
                }
                if matches!(object.type_, hir::Type::Dictionary(_, _)) {
                    return hir::Expression {
                        type_: hir::Type::Int,
                        kind: hir::ExpressionKind::DictionaryLength(Box::new(object)),
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
                // Semantic analysis records ordinary calls. The collection
                // intrinsics below deliberately have no call record, so this
                // lets a user-defined `Get`/`Add`/`RemoveAt` go directly to
                // the resolved-call path without lowering its receiver once
                // just to discover that it is not a List.
                let has_resolved_call = self.model.calls.contains_key(&model_key);
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
                // `Task` is reserved (see `semantic::validate_no_reserved_type_names`),
                // so a validated module's semantic model always recorded a
                // `task_runs` entry for every node this shape check matches.
                if is_task_run_callee(callee) {
                    let resolved = &self.model.task_runs[&model_key];
                    let function = self.callable_symbols[&resolved.function];
                    let return_type = self.callable_results[&function].clone();
                    let parameter_types = self.callable_parameters[&function].clone();
                    let arguments = arguments
                        .iter()
                        .skip(1)
                        .zip(parameter_types)
                        .map(|(argument, target)| convert(self.expression(argument), &target))
                        .collect();
                    return hir::Expression {
                        type_: hir::Type::Task(Box::new(return_type.clone())),
                        kind: hir::ExpressionKind::TaskRun {
                            function,
                            arguments,
                            return_type: Box::new(return_type),
                        },
                    };
                }
                if is_task_wait_all_callee(callee) {
                    let tasks = self.expression(&arguments[0]);
                    let hir::Type::Array(element) = tasks.type_.clone() else {
                        unreachable!("validated Task.WaitAll has an array operand")
                    };
                    let hir::Type::Task(result_type) = *element else {
                        unreachable!("validated Task.WaitAll has Task<T> elements")
                    };
                    return hir::Expression {
                        type_: hir::Type::Array(result_type.clone()),
                        kind: hir::ExpressionKind::TaskWaitAll {
                            tasks: Box::new(tasks),
                            result_type,
                        },
                    };
                }
                if is_task_cancellation_requested_callee(callee) {
                    return hir::Expression {
                        type_: hir::Type::Bool,
                        kind: hir::ExpressionKind::TaskCancellationRequested,
                    };
                }
                if let ast::ExpressionKind::Member { object, name } = &callee.kind
                    && matches!(name.as_str(), "Wait" | "Cancel")
                {
                    let object = self.expression(object);
                    if let hir::Type::Task(result_type) = object.type_.clone() {
                        if name == "Cancel" {
                            return hir::Expression {
                                type_: hir::Type::Bool,
                                kind: hir::ExpressionKind::TaskCancel {
                                    task: Box::new(object),
                                },
                            };
                        }
                        return hir::Expression {
                            type_: (*result_type).clone(),
                            kind: hir::ExpressionKind::TaskWait {
                                task: Box::new(object),
                                result_type,
                            },
                        };
                    }
                }
                if let Some(operation) = self.model.string_operations.get(&model_key).copied() {
                    let ast::ExpressionKind::Member { object, .. } = &callee.kind else {
                        unreachable!("validated string operation has a member receiver");
                    };
                    let receiver = self.expression(object);
                    let arguments = arguments
                        .iter()
                        .map(|argument| self.expression(argument))
                        .collect();
                    let type_ = match operation {
                        hir::StringOperation::Contains
                        | hir::StringOperation::StartsWith
                        | hir::StringOperation::EndsWith => hir::Type::Bool,
                        hir::StringOperation::IndexOf => hir::Type::Int,
                        hir::StringOperation::SubstringFrom
                        | hir::StringOperation::SubstringRange => hir::Type::String,
                        hir::StringOperation::TryParseBool
                        | hir::StringOperation::TryParseInt
                        | hir::StringOperation::TryParseUInt
                        | hir::StringOperation::TryParseLong
                        | hir::StringOperation::TryParseULong
                        | hir::StringOperation::TryParseFloat
                        | hir::StringOperation::TryParseDouble => {
                            let target = operation
                                .parse_target_name()
                                .expect("TryParse* always names a parse target");
                            let option_name =
                                crate::standard_library::option_specialization_name(target);
                            hir::Type::Enum(self.types[option_name.as_str()])
                        }
                    };
                    return hir::Expression {
                        type_,
                        kind: hir::ExpressionKind::StringOperation {
                            operation,
                            receiver: Box::new(receiver),
                            arguments,
                        },
                    };
                }
                if self.model.format_primitives.contains(&model_key) {
                    let ast::ExpressionKind::Member { object, .. } = &callee.kind else {
                        unreachable!("validated ToString call has a member receiver");
                    };
                    let receiver = self.expression(object);
                    return hir::Expression {
                        type_: hir::Type::String,
                        kind: hir::ExpressionKind::FormatPrimitive {
                            primitive: receiver.type_.clone(),
                            receiver: Box::new(receiver),
                        },
                    };
                }
                if !has_resolved_call
                    && let ast::ExpressionKind::Member { object, name } = &callee.kind
                    && name == "Add"
                {
                    let object = self.expression(object);
                    if let hir::Type::List(element_type) = object.type_.clone() {
                        let value = convert(self.expression(&arguments[0]), &element_type);
                        return hir::Expression {
                            type_: hir::Type::Void,
                            kind: hir::ExpressionKind::ListAdd {
                                list: Box::new(object),
                                value: Box::new(value),
                            },
                        };
                    }
                }
                if !has_resolved_call
                    && let ast::ExpressionKind::Member { object, name } = &callee.kind
                    && name == "Set"
                {
                    let object = self.expression(object);
                    if let hir::Type::List(element_type) = object.type_.clone() {
                        return hir::Expression {
                            type_: hir::Type::Void,
                            kind: hir::ExpressionKind::ListSet {
                                list: Box::new(object),
                                index: Box::new(self.expression(&arguments[0])),
                                value: Box::new(convert(
                                    self.expression(&arguments[1]),
                                    &element_type,
                                )),
                            },
                        };
                    }
                }
                if !has_resolved_call
                    && let ast::ExpressionKind::Member { object, name } = &callee.kind
                    && name == "Clear"
                {
                    let object = self.expression(object);
                    if matches!(object.type_, hir::Type::List(_)) {
                        return hir::Expression {
                            type_: hir::Type::Void,
                            kind: hir::ExpressionKind::ListClear {
                                list: Box::new(object),
                            },
                        };
                    }
                }
                if !has_resolved_call
                    && let ast::ExpressionKind::Member { object, name } = &callee.kind
                    && name == "ToArray"
                {
                    let object = self.expression(object);
                    if let hir::Type::List(element_type) = object.type_.clone() {
                        return hir::Expression {
                            type_: hir::Type::Array(element_type.clone()),
                            kind: hir::ExpressionKind::ListToArray {
                                list: Box::new(object),
                                element_type: *element_type,
                            },
                        };
                    }
                }
                if !has_resolved_call
                    && let ast::ExpressionKind::Member { object, name } = &callee.kind
                    && name == "Get"
                {
                    let object = self.expression(object);
                    if let hir::Type::List(element_type) = object.type_.clone() {
                        let index = self.expression(&arguments[0]);
                        return hir::Expression {
                            type_: (*element_type).clone(),
                            kind: hir::ExpressionKind::ListGet {
                                list: Box::new(object),
                                index: Box::new(index),
                                element_type: *element_type,
                            },
                        };
                    }
                }
                if !has_resolved_call
                    && let ast::ExpressionKind::Member { object, name } = &callee.kind
                    && name == "RemoveAt"
                {
                    let object = self.expression(object);
                    if matches!(object.type_, hir::Type::List(_)) {
                        let index = self.expression(&arguments[0]);
                        return hir::Expression {
                            type_: hir::Type::Void,
                            kind: hir::ExpressionKind::ListRemoveAt {
                                list: Box::new(object),
                                index: Box::new(index),
                            },
                        };
                    }
                }
                if let ast::ExpressionKind::Member { object, name } = &callee.kind
                    && self.model.dictionary_operations.contains_key(&model_key)
                {
                    let object = self.expression(object);
                    if let hir::Type::Dictionary(key_type, value_type) = object.type_.clone() {
                        let mut lowered_arguments = arguments
                            .iter()
                            .map(|argument| self.expression(argument))
                            .collect::<Vec<_>>();
                        if let Some(key) = lowered_arguments.first_mut() {
                            *key = convert(key.clone(), &key_type);
                        }
                        if let Some(value) = lowered_arguments.get_mut(1) {
                            *value = convert(value.clone(), &value_type);
                        }
                        let mut lowered_arguments = lowered_arguments.into_iter();
                        let mut key = || {
                            Box::new(
                                lowered_arguments
                                    .next()
                                    .expect("validated Dictionary key argument"),
                            )
                        };
                        return match name.as_str() {
                            "Add" => hir::Expression {
                                type_: hir::Type::Bool,
                                kind: hir::ExpressionKind::DictionaryAdd {
                                    dictionary: Box::new(object),
                                    key: key(),
                                    value: Box::new(
                                        lowered_arguments
                                            .next()
                                            .expect("validated Dictionary value argument"),
                                    ),
                                },
                            },
                            "Set" => hir::Expression {
                                type_: hir::Type::Bool,
                                kind: hir::ExpressionKind::DictionarySet {
                                    dictionary: Box::new(object),
                                    key: key(),
                                    value: Box::new(
                                        lowered_arguments
                                            .next()
                                            .expect("validated Dictionary value argument"),
                                    ),
                                },
                            },
                            "ContainsKey" => hir::Expression {
                                type_: hir::Type::Bool,
                                kind: hir::ExpressionKind::DictionaryContainsKey {
                                    dictionary: Box::new(object),
                                    key: key(),
                                },
                            },
                            "Remove" => hir::Expression {
                                type_: hir::Type::Bool,
                                kind: hir::ExpressionKind::DictionaryRemove {
                                    dictionary: Box::new(object),
                                    key: key(),
                                },
                            },
                            "TryGet" => {
                                let crate::semantic::ResolvedDictionaryOperation::TryGet {
                                    option_type,
                                    some_index,
                                    none_index,
                                } = &self.model.dictionary_operations[&model_key]
                                else {
                                    unreachable!("TryGet carries resolved Option metadata")
                                };
                                let (some_case, some_fields) =
                                    self.enum_cases[&(option_type.clone(), *some_index)].clone();
                                let (none_case, _) =
                                    self.enum_cases[&(option_type.clone(), *none_index)].clone();
                                let some_field = some_fields[0];
                                hir::Expression {
                                    type_: hir::Type::Enum(self.types[option_type]),
                                    kind: hir::ExpressionKind::DictionaryTryGet {
                                        dictionary: Box::new(object),
                                        key: key(),
                                        value_type: (*value_type).clone(),
                                        option_layout: hir::DictionaryOptionLayout {
                                            some_case,
                                            some_field,
                                            some_tag: tag(*some_index),
                                            none_case,
                                            none_tag: tag(*none_index),
                                        },
                                    },
                                }
                            }
                            "Entries" => {
                                let crate::semantic::ResolvedDictionaryOperation::Entries {
                                    entry_type,
                                } = &self.model.dictionary_operations[&model_key]
                                else {
                                    unreachable!("Entries carries resolved entry metadata")
                                };
                                let entry_symbol = self.types[entry_type];
                                let entry_type_hir = hir::Type::User(entry_symbol);
                                hir::Expression {
                                    type_: hir::Type::Array(Box::new(entry_type_hir.clone())),
                                    kind: hir::ExpressionKind::DictionaryEntries {
                                        dictionary: Box::new(object),
                                        key_type: (*key_type).clone(),
                                        value_type: (*value_type).clone(),
                                        entry_type: entry_type_hir,
                                        entry_layout: hir::DictionaryEntryLayout {
                                            key_field: self.members[&entry_symbol]["Key"],
                                            value_field: self.members[&entry_symbol]["Value"],
                                        },
                                    },
                                }
                            }
                            "Clear" => hir::Expression {
                                type_: hir::Type::Void,
                                kind: hir::ExpressionKind::DictionaryClear {
                                    dictionary: Box::new(object),
                                },
                            },
                            "Keys" => hir::Expression {
                                type_: hir::Type::Array(key_type.clone()),
                                kind: hir::ExpressionKind::DictionaryKeys {
                                    dictionary: Box::new(object),
                                    key_type: *key_type,
                                },
                            },
                            "Values" => hir::Expression {
                                type_: hir::Type::Array(value_type.clone()),
                                kind: hir::ExpressionKind::DictionaryValues {
                                    dictionary: Box::new(object),
                                    value_type: *value_type,
                                },
                            },
                            _ => unreachable!("semantic analysis validated Dictionary method"),
                        };
                    }
                }
                if is_parallel_for_callee(callee) {
                    let resolved = &self.model.parallel_for[&model_key];
                    let body = self.callable_symbols[&resolved.body];
                    let [start, end, _body] = arguments.as_slice() else {
                        unreachable!("validated Parallel.For has exactly 3 arguments");
                    };
                    return hir::Expression {
                        type_: hir::Type::Void,
                        kind: hir::ExpressionKind::ParallelFor {
                            start: Box::new(self.expression(start)),
                            end: Box::new(self.expression(end)),
                            body,
                        },
                    };
                }
                if is_parallel_for_each_callee(callee) {
                    let resolved = &self.model.parallel_for_each[&model_key];
                    let body = self.callable_symbols[&resolved.body];
                    let [values, _body] = arguments.as_slice() else {
                        unreachable!("validated Parallel.ForEach has exactly 2 arguments");
                    };
                    let values = self.expression(values);
                    let element_type = match values.type_.clone() {
                        hir::Type::Array(element) => element,
                        other => Box::new(other),
                    };
                    return hir::Expression {
                        type_: hir::Type::Void,
                        kind: hir::ExpressionKind::ParallelForEach {
                            values: Box::new(values),
                            element_type,
                            body,
                        },
                    };
                }
                if is_parallel_reduce_callee(callee) {
                    let resolved = &self.model.parallel_reduce[&model_key];
                    let accumulate = self.callable_symbols[&resolved.accumulate];
                    let combine = self.callable_symbols[&resolved.combine];
                    let [values, identity, _accumulate, _combine] = arguments.as_slice() else {
                        unreachable!("validated Parallel.Reduce has exactly 4 arguments");
                    };
                    let values = self.expression(values);
                    let element_type = match values.type_.clone() {
                        hir::Type::Array(element) => element,
                        other => Box::new(other),
                    };
                    let identity = self.expression(identity);
                    let accumulator_type = identity.type_.clone();
                    return hir::Expression {
                        type_: accumulator_type,
                        kind: hir::ExpressionKind::ParallelReduce {
                            values: Box::new(values),
                            element_type,
                            identity: Box::new(identity),
                            accumulate,
                            combine,
                        },
                    };
                }
                if let (Some(builder), Some(operation)) = (
                    self.string_builder,
                    self.model.string_builder_operations.get(&model_key),
                ) {
                    match operation {
                        crate::semantic::ResolvedStringBuilderOperation::Append => {
                            let ast::ExpressionKind::Member { object, .. } = &callee.kind else {
                                unreachable!("StringBuilder.Append is an instance method")
                            };
                            return hir::Expression {
                                type_: hir::Type::Void,
                                kind: hir::ExpressionKind::StringBuilderAppend {
                                    builder: Box::new(self.expression(object)),
                                    value: Box::new(self.expression(&arguments[0])),
                                    class_symbol: builder.class,
                                },
                            };
                        }
                        crate::semantic::ResolvedStringBuilderOperation::ToString => {
                            let ast::ExpressionKind::Member { object, .. } = &callee.kind else {
                                unreachable!("StringBuilder.ToString is an instance method")
                            };
                            return hir::Expression {
                                type_: hir::Type::String,
                                kind: hir::ExpressionKind::StringBuilderToString {
                                    builder: Box::new(self.expression(object)),
                                    class_symbol: builder.class,
                                },
                            };
                        }
                    }
                }
                let resolved = &self.model.calls[&model_key];
                let symbol = self.callable_symbols[&resolved.callable];
                let type_ = self.callable_results[&symbol].clone();
                let parameter_types = self.callable_parameters[&symbol].clone();
                let plan = self.model.call_arguments[&model_key].clone();
                let mut lowered = arguments
                    .iter()
                    .zip(&plan.source_to_parameter)
                    .map(|(value, parameter)| {
                        let mut argument = self.expression(value);
                        if let Some(target) = parameter_types.get(*parameter) {
                            argument = convert(argument, target);
                        }
                        argument
                    })
                    .collect::<Vec<_>>();
                let mut argument_order = plan.source_to_parameter;
                for default in plan.defaults {
                    lowered.push(convert(
                        constant_expression(&default.value),
                        &parameter_types[default.parameter],
                    ));
                    argument_order.push(default.parameter);
                }
                if self.model.foreign_calls.contains(&model_key) {
                    return hir::Expression {
                        type_,
                        kind: hir::ExpressionKind::ForeignCall {
                            function: symbol,
                            arguments: lowered,
                            argument_order,
                        },
                    };
                }
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
                        arguments: lowered,
                        argument_order,
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
                let operand_key = crate::semantic::ModelNodeKey {
                    context: self.model_context.clone(),
                    span: operand.span,
                };
                if self.model.list_indexes.contains(&operand_key) {
                    let ast::ExpressionKind::Index { array, index } = &operand.kind else {
                        unreachable!("a list index model entry belongs to an index expression")
                    };
                    let list = self.expression(array);
                    let element_type = match &list.type_ {
                        hir::Type::List(element) => (**element).clone(),
                        _ => hir::Type::Unknown,
                    };
                    return hir::Expression {
                        type_: element_type.clone(),
                        kind: hir::ExpressionKind::ListIndexIncrementDecrement {
                            list: Box::new(list),
                            index: Box::new(self.expression(index)),
                            operator: increment(*operator),
                            prefix: *prefix,
                            element_type,
                        },
                    };
                }
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
            ast::ExpressionKind::Await { operand } => {
                let operand = self.expression(operand);
                let result_type = match operand.type_.clone() {
                    hir::Type::Task(inner) => inner,
                    other => Box::new(other),
                };
                hir::Expression {
                    type_: (*result_type).clone(),
                    kind: hir::ExpressionKind::Await {
                        operand: Box::new(operand),
                        result_type,
                    },
                }
            }
            ast::ExpressionKind::Try { operand } => {
                let resolved = self.model.propagations[&model_key].clone();
                let value = self.expression(operand);
                match resolved {
                    crate::semantic::ResolvedPropagation::Result {
                        result_type,
                        ok_index,
                        error_index,
                        function_result_type,
                        function_error_index,
                    } => {
                        let (ok_case, ok_fields) =
                            self.enum_cases[&(result_type.clone(), ok_index)].clone();
                        let (error_case, error_fields) =
                            self.enum_cases[&(result_type, error_index)].clone();
                        let (return_error_case, return_error_fields) =
                            self.enum_cases[&(function_result_type, function_error_index)].clone();
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
                                ok_tag: tag(ok_index),
                                error_case,
                                error_field,
                                error_tag: tag(error_index),
                                return_type: self.current_return.clone(),
                                return_error_case,
                                return_error_field,
                                return_error_tag: tag(function_error_index),
                            },
                        }
                    }
                    crate::semantic::ResolvedPropagation::Option {
                        option_type,
                        some_index,
                        none_index,
                        function_option_type,
                        function_none_index,
                    } => {
                        let (some_case, some_fields) =
                            self.enum_cases[&(option_type.clone(), some_index)].clone();
                        let (return_none_case, _) =
                            self.enum_cases[&(function_option_type, function_none_index)].clone();
                        let some_field = some_fields[0];
                        let success_type = self.symbol_types[&some_field].clone();
                        hir::Expression {
                            type_: success_type.clone(),
                            kind: hir::ExpressionKind::PropagateOption {
                                operand: Box::new(value),
                                success_type,
                                some_case,
                                some_field,
                                some_tag: tag(some_index),
                                none_tag: tag(none_index),
                                return_type: self.current_return.clone(),
                                return_none_case,
                                return_none_tag: tag(function_none_index),
                            },
                        }
                    }
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
            ast::ExpressionKind::Switch {
                value,
                cases,
                default,
            } => {
                let value = self.expression(value);
                let mut lowered_cases = Vec::new();
                for case in cases {
                    self.scopes.push(std::collections::HashMap::new());
                    let (case_symbol, tag, bindings) =
                        self.switch_pattern(case.span, &case.bindings);
                    let arm = self.expression(&case.value);
                    self.scopes.pop();
                    lowered_cases.push(hir::SwitchExpressionCase {
                        case: case_symbol,
                        tag,
                        bindings,
                        value: arm,
                    });
                }
                let mut lowered_default = default.as_ref().map(|arm| self.expression(arm));
                let result_type_name = self.model.switch_expression_types[&model_key].clone();
                let result_type =
                    self.resolve_type(&ast::TypeRef::new(result_type_name, expression.span));
                for case in &mut lowered_cases {
                    case.value = convert(case.value.clone(), &result_type);
                }
                lowered_default = lowered_default.map(|arm| convert(arm, &result_type));
                hir::Expression {
                    type_: result_type,
                    kind: hir::ExpressionKind::Switch {
                        value: Box::new(value),
                        cases: lowered_cases,
                        default: lowered_default.map(Box::new),
                    },
                }
            }
            ast::ExpressionKind::Binary { .. } => self.binary_expression(expression),
            ast::ExpressionKind::Assignment {
                target,
                operator,
                value,
            } => {
                let target_key = crate::semantic::ModelNodeKey {
                    context: self.model_context.clone(),
                    span: target.span,
                };
                if self.model.list_indexes.contains(&target_key) {
                    let ast::ExpressionKind::Index { array, index } = &target.kind else {
                        unreachable!("a list index model entry belongs to an index expression")
                    };
                    let list = self.expression(array);
                    let element_type = match &list.type_ {
                        hir::Type::List(element) => (**element).clone(),
                        _ => hir::Type::Unknown,
                    };
                    return hir::Expression {
                        type_: element_type.clone(),
                        kind: hir::ExpressionKind::ListIndexAssignment {
                            list: Box::new(list),
                            index: Box::new(self.expression(index)),
                            operator: assignment(*operator),
                            value: Box::new(convert(self.expression(value), &element_type)),
                            element_type,
                        },
                    };
                }
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
            ast::ExpressionKind::InterpolatedString { parts } => {
                let parts = parts
                    .iter()
                    .map(|part| match part {
                        ast::InterpolatedPart::Text(text) => {
                            hir::InterpolatedPart::Text(text.clone())
                        }
                        ast::InterpolatedPart::Expression(expression) => {
                            hir::InterpolatedPart::Expression(Box::new(self.expression(expression)))
                        }
                    })
                    .collect();
                hir::Expression {
                    type_: hir::Type::String,
                    kind: hir::ExpressionKind::InterpolatedString { parts },
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
                argument_order: Vec::new(),
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

/// Mirrors `semantic::general::calls::is_task_run_callee` structurally at
/// this layer, exactly like `log_level` mirrors `logging_level`.
fn is_task_run_callee(callee: &ast::Expression) -> bool {
    matches!(
        &callee.kind,
        ast::ExpressionKind::Member { object, name }
            if name == "Run"
                && matches!(&object.kind, ast::ExpressionKind::Name(object) if object == "Task")
    )
}

fn is_task_wait_all_callee(callee: &ast::Expression) -> bool {
    matches!(
        &callee.kind,
        ast::ExpressionKind::Member { object, name }
            if name == "WaitAll"
                && matches!(&object.kind, ast::ExpressionKind::Name(object) if object == "Task")
    )
}

fn is_task_cancellation_requested_callee(callee: &ast::Expression) -> bool {
    matches!(
        &callee.kind,
        ast::ExpressionKind::Member { object, name }
            if name == "IsCancellationRequested"
                && matches!(&object.kind, ast::ExpressionKind::Name(object) if object == "Task")
    )
}

/// Mirrors `semantic::general::calls::is_parallel_for_callee`.
fn is_parallel_for_callee(callee: &ast::Expression) -> bool {
    matches!(
        &callee.kind,
        ast::ExpressionKind::Member { object, name }
            if name == "For"
                && matches!(&object.kind, ast::ExpressionKind::Name(object) if object == "Parallel")
    )
}

/// Mirrors `semantic::general::calls::is_parallel_for_each_callee`.
fn is_parallel_for_each_callee(callee: &ast::Expression) -> bool {
    matches!(
        &callee.kind,
        ast::ExpressionKind::Member { object, name }
            if name == "ForEach"
                && matches!(&object.kind, ast::ExpressionKind::Name(object) if object == "Parallel")
    )
}

/// Mirrors `semantic::general::calls::is_parallel_reduce_callee`.
fn is_parallel_reduce_callee(callee: &ast::Expression) -> bool {
    matches!(
        &callee.kind,
        ast::ExpressionKind::Member { object, name }
            if name == "Reduce"
                && matches!(&object.kind, ast::ExpressionKind::Name(object) if object == "Parallel")
    )
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
