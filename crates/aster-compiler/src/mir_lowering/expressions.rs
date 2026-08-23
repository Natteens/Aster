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
                // Named payloads remain in source order through HIR so their
                // expressions retain ordinary left-to-right evaluation. MIR's
                // enum layout is declaration ordered, so materialize every
                // value first and only then permute the inert operands.
                let declared = self
                    .enum_cases
                    .get(case)
                    .expect("validated enum case has a declaration")
                    .fields
                    .clone();
                let mut ordered = std::iter::repeat_with(|| None)
                    .take(declared.len())
                    .collect::<Vec<Option<mir::FieldOperand>>>();
                for field in fields {
                    let value = self
                        .lower_expression(&field.value)
                        .expect("validated enum payload produces a value");
                    let position = declared
                        .iter()
                        .position(|candidate| candidate.symbol == field.field)
                        .expect("validated enum payload names a declared field");
                    assert!(
                        ordered[position]
                            .replace(mir::FieldOperand {
                                field: field.field,
                                value,
                            })
                            .is_none(),
                        "validated enum payload initializes each field once"
                    );
                }
                let fields = ordered
                    .into_iter()
                    .map(|field| field.expect("validated enum payload initializes every field"))
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
                argument_order,
            } => Some(self.lower_new_object(
                &expression.type_,
                *class_symbol,
                *constructor,
                arguments,
                argument_order,
            )),
            hir::ExpressionKind::ArrayLiteral(elements) => {
                Some(self.lower_array_literal(&expression.type_, elements))
            }
            hir::ExpressionKind::NewArray {
                element_type,
                length,
                initialization,
            } => {
                Some(self.lower_new_array(&expression.type_, element_type, length, *initialization))
            }
            hir::ExpressionKind::NewList { element_type } => {
                Some(self.lower_new_list(&expression.type_, element_type))
            }
            hir::ExpressionKind::NewDictionary {
                key_type,
                value_type,
            } => Some(self.lower_new_dictionary(&expression.type_, key_type, value_type)),
            hir::ExpressionKind::NewStringBuilder { class_symbol } => {
                Some(self.lower_new_string_builder(&expression.type_, *class_symbol))
            }
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
            hir::ExpressionKind::ListLength(list) => {
                let list = self
                    .lower_expression(list)
                    .expect("validated list produces a value");
                Some(self.temporary(expression.type_.clone(), mir::RvalueKind::ListLength(list)))
            }
            hir::ExpressionKind::ListCapacity(list) => {
                let list = self
                    .lower_expression(list)
                    .expect("validated list produces a value");
                let local = self.new_temporary(hir::Type::Int);
                let destination = mir::Place::Local(local);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::ListCapacity,
                    arguments: vec![list],
                    return_type: mir::Type::Int,
                });
                Some(mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::ExpressionKind::DictionaryLength(dictionary) => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary produces a value");
                Some(self.temporary(
                    expression.type_.clone(),
                    mir::RvalueKind::DictionaryLength(dictionary),
                ))
            }
            hir::ExpressionKind::DictionaryCapacity(dictionary) => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary produces a value");
                let local = self.new_temporary(hir::Type::Int);
                let destination = mir::Place::Local(local);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::DictionaryCapacity,
                    arguments: vec![dictionary],
                    return_type: mir::Type::Int,
                });
                Some(mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::ExpressionKind::DictionaryEnsureCapacity {
                dictionary,
                minimum,
            } => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary");
                let minimum = self.lower_expression(minimum).expect("validated minimum");
                let destination = mir::Place::Local(self.new_temporary(hir::Type::Int));
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::DictionaryEnsureCapacity,
                    arguments: vec![dictionary, minimum],
                    return_type: mir::Type::Int,
                });
                Some(mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::ExpressionKind::DictionaryGetOr {
                dictionary,
                key,
                fallback,
            }
            | hir::ExpressionKind::DictionaryGetOrAdd {
                dictionary,
                key,
                value: fallback,
            } => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary");
                let key = self.lower_expression(key).expect("validated key");
                let fallback = self.lower_expression(fallback).expect("validated value");
                let destination = mir::Place::Local(self.new_temporary(expression.type_.clone()));
                let intrinsic = if matches!(
                    &expression.kind,
                    hir::ExpressionKind::DictionaryGetOrAdd { .. }
                ) {
                    mir::Intrinsic::DictionaryGetOrAdd
                } else {
                    mir::Intrinsic::DictionaryGetOr
                };
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic,
                    arguments: vec![dictionary, key, fallback],
                    return_type: expression.type_.clone(),
                });
                Some(mir::Operand {
                    type_: expression.type_.clone(),
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::ExpressionKind::ListEnsureCapacity { list, minimum } => {
                let list_value = self.lower_expression(list).expect("validated list");
                let minimum = self.lower_expression(minimum).expect("validated minimum");
                let element_type = match &list.type_ {
                    hir::Type::List(element) => (**element).clone(),
                    _ => hir::Type::Unknown,
                };
                let local = self.new_temporary(hir::Type::Int);
                let destination = mir::Place::Local(local);
                let _ = element_type;
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::ListEnsureCapacity,
                    arguments: vec![list_value, minimum],
                    return_type: mir::Type::Int,
                });
                Some(mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::ExpressionKind::ListAddRange {
                list,
                values,
                element_type,
            } => {
                let list = self.lower_expression(list).expect("validated list");
                let values = self.lower_expression(values).expect("validated values");
                let _ = element_type;
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic: mir::Intrinsic::ListAddRange,
                    arguments: vec![list, values],
                    return_type: mir::Type::Void,
                });
                None
            }
            hir::ExpressionKind::ListInsert { list, index, value } => {
                let element_type = value.type_.clone();
                let list = self.lower_expression(list).expect("validated list");
                let index = self.lower_expression(index).expect("validated index");
                let value = self.lower_expression(value).expect("validated value");
                let _ = element_type;
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic: mir::Intrinsic::ListInsert,
                    arguments: vec![list, index, value],
                    return_type: mir::Type::Void,
                });
                None
            }
            hir::ExpressionKind::ListRemoveRange { list, index, count } => {
                let element_type = match &list.type_ {
                    hir::Type::List(element) => (**element).clone(),
                    _ => hir::Type::Unknown,
                };
                let list = self.lower_expression(list).expect("validated list");
                let index = self.lower_expression(index).expect("validated index");
                let count = self.lower_expression(count).expect("validated count");
                let _ = element_type;
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic: mir::Intrinsic::ListRemoveRange,
                    arguments: vec![list, index, count],
                    return_type: mir::Type::Void,
                });
                None
            }
            hir::ExpressionKind::ListReverse { list } => {
                let element_type = match &list.type_ {
                    hir::Type::List(element) => (**element).clone(),
                    _ => hir::Type::Unknown,
                };
                let list = self.lower_expression(list).expect("validated list");
                let _ = element_type;
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic: mir::Intrinsic::ListReverse,
                    arguments: vec![list],
                    return_type: mir::Type::Void,
                });
                None
            }
            hir::ExpressionKind::ListGetRange {
                list,
                index,
                count,
                element_type,
            } => {
                let list = self.lower_expression(list).expect("validated list");
                let index = self.lower_expression(index).expect("validated index");
                let count = self.lower_expression(count).expect("validated count");
                let local = self.new_temporary(hir::Type::Array(Box::new(element_type.clone())));
                let destination = mir::Place::Local(local);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::ListGetRange,
                    arguments: vec![list, index, count],
                    return_type: mir::Type::Array(Box::new(element_type.clone())),
                });
                Some(mir::Operand {
                    type_: mir::Type::Array(Box::new(element_type.clone())),
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::ExpressionKind::StringBuilderAppend {
                builder,
                value,
                class_symbol,
            } => {
                let builder = self
                    .lower_expression(builder)
                    .expect("validated StringBuilder receiver produces a value");
                let value = self
                    .lower_expression(value)
                    .expect("validated string append value produces a value");
                self.instruction(mir::Instruction::StringBuilderAppend {
                    builder,
                    value,
                    class: *class_symbol,
                });
                None
            }
            hir::ExpressionKind::StringBuilderLength { builder, .. } => {
                let builder = self
                    .lower_expression(builder)
                    .expect("validated builder produces a value");
                let local = self.new_temporary(hir::Type::Int);
                let destination = mir::Place::Local(local);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::StringBuilderLength,
                    arguments: vec![builder],
                    return_type: mir::Type::Int,
                });
                Some(mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::ExpressionKind::StringBuilderClear { builder, .. } => {
                let builder = self
                    .lower_expression(builder)
                    .expect("validated builder produces a value");
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic: mir::Intrinsic::StringBuilderClear,
                    arguments: vec![builder],
                    return_type: mir::Type::Void,
                });
                None
            }
            hir::ExpressionKind::StringBuilderToString {
                builder,
                class_symbol,
            } => {
                let builder = self
                    .lower_expression(builder)
                    .expect("validated StringBuilder receiver produces a value");
                let destination = self.new_temporary(hir::Type::String);
                let place = mir::Place::Local(destination);
                self.instruction(mir::Instruction::StringBuilderToString {
                    destination: place.clone(),
                    builder,
                    class: *class_symbol,
                    region: mir::AllocationRegion::Persistent,
                });
                Some(mir::Operand {
                    type_: mir::Type::String,
                    kind: mir::OperandKind::Copy(place),
                })
            }
            hir::ExpressionKind::DictionaryAdd {
                dictionary,
                key,
                value,
            }
            | hir::ExpressionKind::DictionarySet {
                dictionary,
                key,
                value,
            } => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary produces a value");
                let key = self
                    .lower_expression(key)
                    .expect("validated dictionary key produces a value");
                let value = self
                    .lower_expression(value)
                    .expect("validated dictionary value produces a value");
                let destination = self.new_temporary(hir::Type::Bool);
                let place = mir::Place::Local(destination);
                let instruction =
                    if matches!(&expression.kind, hir::ExpressionKind::DictionaryAdd { .. }) {
                        mir::Instruction::DictionaryAdd {
                            destination: place.clone(),
                            dictionary,
                            key,
                            value,
                        }
                    } else {
                        mir::Instruction::DictionarySet {
                            destination: place.clone(),
                            dictionary,
                            key,
                            value,
                        }
                    };
                self.instruction(instruction);
                Some(mir::Operand {
                    type_: mir::Type::Bool,
                    kind: mir::OperandKind::Copy(place),
                })
            }
            hir::ExpressionKind::DictionaryTryGet {
                dictionary,
                key,
                value_type,
                option_layout,
            } => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary produces a value");
                let key = self
                    .lower_expression(key)
                    .expect("validated dictionary key produces a value");
                let destination = self.new_temporary(expression.type_.clone());
                let place = mir::Place::Local(destination);
                self.instruction(mir::Instruction::DictionaryTryGet {
                    destination: place.clone(),
                    dictionary,
                    key,
                    value_type: value_type.clone(),
                    option_layout: *option_layout,
                });
                Some(mir::Operand {
                    type_: expression.type_.clone(),
                    kind: mir::OperandKind::Copy(place),
                })
            }
            hir::ExpressionKind::DictionaryContainsKey { dictionary, key }
            | hir::ExpressionKind::DictionaryRemove { dictionary, key } => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary produces a value");
                let key = self
                    .lower_expression(key)
                    .expect("validated dictionary key produces a value");
                let destination = self.new_temporary(hir::Type::Bool);
                let place = mir::Place::Local(destination);
                let instruction = if matches!(
                    &expression.kind,
                    hir::ExpressionKind::DictionaryContainsKey { .. }
                ) {
                    mir::Instruction::DictionaryContainsKey {
                        destination: place.clone(),
                        dictionary,
                        key,
                    }
                } else {
                    mir::Instruction::DictionaryRemove {
                        destination: place.clone(),
                        dictionary,
                        key,
                    }
                };
                self.instruction(instruction);
                Some(mir::Operand {
                    type_: mir::Type::Bool,
                    kind: mir::OperandKind::Copy(place),
                })
            }
            hir::ExpressionKind::DictionaryEntries {
                dictionary,
                key_type,
                value_type,
                entry_type,
                entry_layout,
            } => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary produces a value");
                let destination = self.new_temporary(expression.type_.clone());
                let place = mir::Place::Local(destination);
                self.instruction(mir::Instruction::DictionaryEntries {
                    destination: place.clone(),
                    dictionary,
                    key_type: key_type.clone(),
                    value_type: value_type.clone(),
                    entry_type: entry_type.clone(),
                    entry_layout: *entry_layout,
                    region: mir::AllocationRegion::Persistent,
                });
                Some(mir::Operand {
                    type_: expression.type_.clone(),
                    kind: mir::OperandKind::Copy(place),
                })
            }
            hir::ExpressionKind::DictionaryClear { dictionary } => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary produces a value");
                self.instruction(mir::Instruction::DictionaryClear { dictionary });
                None
            }
            hir::ExpressionKind::DictionaryKeys {
                dictionary,
                key_type,
            } => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary produces a value");
                let destination = self.new_temporary(expression.type_.clone());
                let place = mir::Place::Local(destination);
                self.instruction(mir::Instruction::DictionaryKeys {
                    destination: place.clone(),
                    dictionary,
                    key_type: key_type.clone(),
                    region: mir::AllocationRegion::Persistent,
                });
                Some(mir::Operand {
                    type_: expression.type_.clone(),
                    kind: mir::OperandKind::Copy(place),
                })
            }
            hir::ExpressionKind::DictionaryValues {
                dictionary,
                value_type,
            } => {
                let dictionary = self
                    .lower_expression(dictionary)
                    .expect("validated dictionary produces a value");
                let destination = self.new_temporary(expression.type_.clone());
                let place = mir::Place::Local(destination);
                self.instruction(mir::Instruction::DictionaryValues {
                    destination: place.clone(),
                    dictionary,
                    value_type: value_type.clone(),
                    region: mir::AllocationRegion::Persistent,
                });
                Some(mir::Operand {
                    type_: expression.type_.clone(),
                    kind: mir::OperandKind::Copy(place),
                })
            }
            hir::ExpressionKind::ListAdd { list, value } => {
                let list = self
                    .lower_expression(list)
                    .expect("validated list produces a value");
                let value = self
                    .lower_expression(value)
                    .expect("validated value produces a value");
                self.instruction(mir::Instruction::ListAdd { list, value });
                None
            }
            hir::ExpressionKind::ListGet {
                list,
                index,
                element_type,
            } => {
                let list = self
                    .lower_expression(list)
                    .expect("validated list produces a value");
                let index = self
                    .lower_expression(index)
                    .expect("validated index produces a value");
                Some(self.lower_list_get(list, index, element_type))
            }
            hir::ExpressionKind::ListRemoveAt { list, index } => {
                let list = self
                    .lower_expression(list)
                    .expect("validated list produces a value");
                let index = self
                    .lower_expression(index)
                    .expect("validated index produces a value");
                self.instruction(mir::Instruction::ListRemoveAt { list, index });
                None
            }
            hir::ExpressionKind::ListSet { list, index, value } => {
                let list = self
                    .lower_expression(list)
                    .expect("validated list produces a value");
                let index = self
                    .lower_expression(index)
                    .expect("validated index produces a value");
                let value = self
                    .lower_expression(value)
                    .expect("validated value produces a value");
                self.instruction(mir::Instruction::ListSet { list, index, value });
                None
            }
            hir::ExpressionKind::ListIndexAssignment {
                list,
                index,
                operator,
                value,
                element_type,
            } => {
                Some(self.lower_list_index_assignment(list, index, *operator, value, element_type))
            }
            hir::ExpressionKind::ListIndexIncrementDecrement {
                list,
                index,
                operator,
                prefix,
                element_type,
            } => Some(self.lower_list_index_increment_decrement(
                list,
                index,
                *operator,
                *prefix,
                element_type,
            )),
            hir::ExpressionKind::ListClear { list } => {
                let list = self
                    .lower_expression(list)
                    .expect("validated list produces a value");
                self.instruction(mir::Instruction::ListClear { list });
                None
            }
            hir::ExpressionKind::ListToArray { list, element_type } => {
                let list = self
                    .lower_expression(list)
                    .expect("validated list produces a value");
                let destination = self.new_temporary(expression.type_.clone());
                let place = mir::Place::Local(destination);
                self.instruction(mir::Instruction::ListToArray {
                    destination: place.clone(),
                    list,
                    element_type: element_type.clone(),
                    region: mir::AllocationRegion::Persistent,
                });
                Some(mir::Operand {
                    type_: expression.type_.clone(),
                    kind: mir::OperandKind::Copy(place),
                })
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
            hir::ExpressionKind::StringOperation {
                operation,
                receiver,
                arguments,
            } => {
                let receiver = self
                    .lower_expression(receiver)
                    .expect("validated string receiver produces a value");
                let mut lowered = Vec::with_capacity(arguments.len() + 1);
                lowered.push(receiver);
                lowered.extend(arguments.iter().map(|argument| {
                    self.lower_expression(argument)
                        .expect("validated string argument produces a value")
                }));
                let intrinsic = match operation {
                    hir::StringOperation::Contains => mir::Intrinsic::StringContains,
                    hir::StringOperation::StartsWith => mir::Intrinsic::StringStartsWith,
                    hir::StringOperation::EndsWith => mir::Intrinsic::StringEndsWith,
                    hir::StringOperation::IndexOf => mir::Intrinsic::StringIndexOf,
                    hir::StringOperation::SubstringFrom => mir::Intrinsic::StringSubstringFrom,
                    hir::StringOperation::SubstringRange => mir::Intrinsic::StringSubstringRange,
                    hir::StringOperation::TryParseBool => mir::Intrinsic::StringTryParseBool,
                    hir::StringOperation::TryParseChar => mir::Intrinsic::StringTryParseChar,
                    hir::StringOperation::TryParseSByte => mir::Intrinsic::StringTryParseSByte,
                    hir::StringOperation::TryParseByte => mir::Intrinsic::StringTryParseByte,
                    hir::StringOperation::TryParseShort => mir::Intrinsic::StringTryParseShort,
                    hir::StringOperation::TryParseUShort => mir::Intrinsic::StringTryParseUShort,
                    hir::StringOperation::TryParseInt => mir::Intrinsic::StringTryParseInt,
                    hir::StringOperation::TryParseUInt => mir::Intrinsic::StringTryParseUInt,
                    hir::StringOperation::TryParseLong => mir::Intrinsic::StringTryParseLong,
                    hir::StringOperation::TryParseULong => mir::Intrinsic::StringTryParseULong,
                    hir::StringOperation::TryParseFloat => mir::Intrinsic::StringTryParseFloat,
                    hir::StringOperation::TryParseDouble => mir::Intrinsic::StringTryParseDouble,
                };
                let destination = self.new_temporary(expression.type_.clone());
                let place = mir::Place::Local(destination);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(place.clone()),
                    intrinsic,
                    arguments: lowered,
                    return_type: expression.type_.clone(),
                });
                Some(mir::Operand {
                    type_: expression.type_.clone(),
                    kind: mir::OperandKind::Copy(place),
                })
            }
            hir::ExpressionKind::FormatPrimitive { receiver, .. } => {
                let receiver = self
                    .lower_expression(receiver)
                    .expect("validated ToString receiver produces a value");
                Some(self.stringify(receiver))
            }
            hir::ExpressionKind::Call {
                callee,
                arguments,
                argument_order,
            } => self.lower_call(callee, arguments, argument_order, &expression.type_),
            hir::ExpressionKind::ForeignCall {
                function,
                arguments,
                argument_order,
            } => {
                let arguments = self.lower_ordered_arguments(arguments, argument_order);
                let destination = if expression.type_ == hir::Type::Void {
                    None
                } else {
                    Some(mir::Place::Local(
                        self.new_temporary(expression.type_.clone()),
                    ))
                };
                self.instruction(mir::Instruction::ForeignCall {
                    destination: destination.clone(),
                    function: *function,
                    arguments,
                    return_type: expression.type_.clone(),
                });
                destination.map(|place| mir::Operand {
                    type_: expression.type_.clone(),
                    kind: mir::OperandKind::Copy(place),
                })
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
            hir::ExpressionKind::Switch {
                value,
                cases,
                default,
            } => Some(self.lower_switch_expression(
                value,
                cases,
                default.as_deref(),
                &expression.type_,
            )),
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
            hir::ExpressionKind::PropagateOption {
                operand,
                success_type,
                some_case,
                some_field,
                none_tag,
                return_type,
                return_none_case,
                return_none_tag,
                ..
            } => Some(self.lower_propagate_option(
                operand,
                success_type,
                *some_case,
                *some_field,
                *none_tag,
                return_type,
                *return_none_case,
                *return_none_tag,
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
                    let mut parts = Vec::new();
                    if (is_string_concat(left) || is_string_concat(right))
                        && collect_static_string_concat(expression, &mut parts)
                        && parts.len() >= 3
                    {
                        let segments = parts
                            .into_iter()
                            .map(|part| {
                                self.lower_expression(part)
                                    .expect("validated string operand produces a value")
                            })
                            .collect();
                        return Some(self.emit_string_join(segments));
                    }
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
            hir::ExpressionKind::InterpolatedString { parts } => {
                Some(self.lower_interpolated_string(parts))
            }
            hir::ExpressionKind::TaskRun {
                function,
                arguments,
                return_type,
            } => Some(self.lower_task_run(
                *function,
                arguments,
                return_type,
                expression.type_.clone(),
            )),
            hir::ExpressionKind::TaskWait { task, result_type } => {
                Some(self.lower_task_wait(task, result_type))
            }
            hir::ExpressionKind::TaskWaitAll { tasks, result_type } => {
                Some(self.lower_task_wait_all(tasks, result_type))
            }
            hir::ExpressionKind::TaskCancel { task } => Some(self.lower_task_cancel(task)),
            hir::ExpressionKind::TaskCancellationRequested => {
                Some(self.lower_task_cancellation_requested())
            }
            // Inside a generated async `MoveNext`, the single `await` is
            // lowered to a use of the result the state-1 prologue already
            // materialized from the completed inner task (see `async_machine`).
            // `await` never appears anywhere else in valid Aster.
            hir::ExpressionKind::Await { .. } => self.async_await_result.clone(),
            hir::ExpressionKind::ParallelFor { start, end, body } => {
                self.lower_parallel_for(start, end, *body);
                None
            }
            hir::ExpressionKind::ParallelForEach {
                values,
                element_type,
                body,
            } => {
                self.lower_parallel_for_each(values, element_type, *body);
                None
            }
            hir::ExpressionKind::ParallelReduce {
                values,
                element_type,
                identity,
                accumulate,
                combine,
            } => Some(self.lower_parallel_reduce(
                values,
                element_type,
                identity,
                *accumulate,
                *combine,
                expression.type_.clone(),
            )),
        }
    }

    /// `aster.core.Task.Run(function, arguments...)`: `function` is already a resolved,
    /// concrete symbol, carried as an `OperandKind::Function` argument
    /// rather than looked up again by name.
    fn lower_task_run(
        &mut self,
        function: hir::SymbolId,
        arguments: &[hir::Expression],
        return_type: &hir::Type,
        task_type: hir::Type,
    ) -> mir::Operand {
        let destination = mir::Place::Local(self.new_temporary(task_type.clone()));
        let mut lowered = Vec::with_capacity(arguments.len() + 1);
        lowered.push(mir::Operand {
            type_: return_type.clone(),
            kind: mir::OperandKind::Function(function),
        });
        lowered.extend(arguments.iter().map(|argument| {
            self.lower_expression(argument)
                .expect("a Task.Run value argument produces an operand")
        }));
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(destination.clone()),
            intrinsic: mir::Intrinsic::TaskRun,
            arguments: lowered,
            return_type: task_type.clone(),
        });
        mir::Operand {
            type_: task_type,
            kind: mir::OperandKind::Copy(destination),
        }
    }

    /// `task.Wait()`: block on the already-lowered `Task<T>` operand and
    /// produce its `T` result.
    fn lower_task_wait(&mut self, task: &hir::Expression, result_type: &hir::Type) -> mir::Operand {
        let task_operand = self
            .lower_expression(task)
            .expect("a Task<T> value produces an operand");
        let destination = mir::Place::Local(self.new_temporary(result_type.clone()));
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(destination.clone()),
            intrinsic: mir::Intrinsic::TaskWait,
            arguments: vec![task_operand],
            return_type: result_type.clone(),
        });
        mir::Operand {
            type_: result_type.clone(),
            kind: mir::OperandKind::Copy(destination),
        }
    }

    fn lower_task_wait_all(
        &mut self,
        tasks: &hir::Expression,
        result_type: &hir::Type,
    ) -> mir::Operand {
        let tasks = self
            .lower_expression(tasks)
            .expect("a Task.WaitAll array produces an operand");
        let array_type = hir::Type::Array(Box::new(result_type.clone()));
        let destination = mir::Place::Local(self.new_temporary(array_type.clone()));
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(destination.clone()),
            intrinsic: mir::Intrinsic::TaskWaitAll,
            arguments: vec![tasks],
            return_type: array_type.clone(),
        });
        mir::Operand {
            type_: array_type,
            kind: mir::OperandKind::Copy(destination),
        }
    }

    fn lower_task_cancel(&mut self, task: &hir::Expression) -> mir::Operand {
        let task = self
            .lower_expression(task)
            .expect("a Task<T> value produces an operand");
        let destination = mir::Place::Local(self.new_temporary(hir::Type::Bool));
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(destination.clone()),
            intrinsic: mir::Intrinsic::TaskCancel,
            arguments: vec![task],
            return_type: hir::Type::Bool,
        });
        mir::Operand {
            type_: hir::Type::Bool,
            kind: mir::OperandKind::Copy(destination),
        }
    }

    fn lower_task_cancellation_requested(&mut self) -> mir::Operand {
        let destination = mir::Place::Local(self.new_temporary(hir::Type::Bool));
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(destination.clone()),
            intrinsic: mir::Intrinsic::TaskCancellationRequested,
            arguments: Vec::new(),
            return_type: hir::Type::Bool,
        });
        mir::Operand {
            type_: hir::Type::Bool,
            kind: mir::OperandKind::Copy(destination),
        }
    }

    /// `Parallel.For(start, end, Body)`: a single synchronous intrinsic. The
    /// range bounds are evaluated left to right, exactly once; `body` is a
    /// resolved symbol carried by identity, never re-resolved by name.
    fn lower_parallel_for(
        &mut self,
        start: &hir::Expression,
        end: &hir::Expression,
        body: hir::SymbolId,
    ) {
        let start = self
            .lower_expression(start)
            .expect("validated range start produces a value");
        let end = self
            .lower_expression(end)
            .expect("validated range end produces a value");
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: None,
            intrinsic: mir::Intrinsic::ParallelFor,
            arguments: vec![
                start,
                end,
                mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Function(body),
                },
            ],
            return_type: mir::Type::Void,
        });
    }

    /// `Parallel.ForEach(values, Body)`: evaluate the scalar array once, then a
    /// single synchronous intrinsic copies its elements host-side and runs
    /// `body` over the copies. `body` is carried by identity.
    fn lower_parallel_for_each(
        &mut self,
        values: &hir::Expression,
        element_type: &hir::Type,
        body: hir::SymbolId,
    ) {
        let values = self
            .lower_expression(values)
            .expect("validated array expression produces a value");
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: None,
            intrinsic: mir::Intrinsic::ParallelForEach,
            arguments: vec![
                values,
                mir::Operand {
                    type_: element_type.clone(),
                    kind: mir::OperandKind::Function(body),
                },
            ],
            return_type: mir::Type::Void,
        });
    }

    /// `Parallel.Reduce(values, identity, Accumulate, Combine)`: evaluate the
    /// scalar array and the identity exactly once, left to right, then a
    /// single synchronous intrinsic copies the array's elements host-side,
    /// folds `Accumulate` per chunk starting from `identity`, and combines
    /// chunk partials with `Combine`. `accumulate`/`combine` are carried by
    /// identity, never re-resolved by name.
    fn lower_parallel_reduce(
        &mut self,
        values: &hir::Expression,
        element_type: &hir::Type,
        identity: &hir::Expression,
        accumulate: hir::SymbolId,
        combine: hir::SymbolId,
        accumulator_type: hir::Type,
    ) -> mir::Operand {
        let values = self
            .lower_expression(values)
            .expect("validated array expression produces a value");
        let identity = self
            .lower_expression(identity)
            .expect("validated identity expression produces a value");
        let destination = mir::Place::Local(self.new_temporary(accumulator_type.clone()));
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(destination.clone()),
            intrinsic: mir::Intrinsic::ParallelReduce,
            arguments: vec![
                values,
                identity,
                mir::Operand {
                    type_: element_type.clone(),
                    kind: mir::OperandKind::Function(accumulate),
                },
                mir::Operand {
                    type_: accumulator_type.clone(),
                    kind: mir::OperandKind::Function(combine),
                },
            ],
            return_type: accumulator_type.clone(),
        });
        mir::Operand {
            type_: accumulator_type,
            kind: mir::OperandKind::Copy(destination),
        }
    }

    /// Lowers `$"..."` to: evaluate and (when needed) stringify every
    /// embedded expression exactly once, left to right, then join every
    /// segment in a single runtime call. Empty literal text segments (the
    /// common case of text before the first `{` or after the last `}`) are
    /// dropped before lowering; they carry no value and would only add an
    /// unnecessary argument to the join call.
    fn lower_interpolated_string(&mut self, parts: &[hir::InterpolatedPart]) -> mir::Operand {
        let mut segments = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                hir::InterpolatedPart::Text(text) => {
                    if text.is_empty() {
                        continue;
                    }
                    segments.push(mir::Operand {
                        type_: mir::Type::String,
                        kind: mir::OperandKind::Constant(mir::Constant::String(text.clone())),
                    });
                }
                hir::InterpolatedPart::Expression(expression) => {
                    let operand = self
                        .lower_expression(expression)
                        .expect("validated interpolation part produces a value");
                    segments.push(self.stringify(operand));
                }
            }
        }
        if segments.is_empty() {
            return mir::Operand {
                type_: mir::Type::String,
                kind: mir::OperandKind::Constant(mir::Constant::String(String::new())),
            };
        }
        self.emit_string_join(segments)
    }

    fn emit_string_join(&mut self, segments: Vec<mir::Operand>) -> mir::Operand {
        let destination = self.new_temporary(hir::Type::String);
        let place = mir::Place::Local(destination);
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(place.clone()),
            intrinsic: mir::Intrinsic::StringJoin,
            arguments: segments,
            return_type: mir::Type::String,
        });
        mir::Operand {
            type_: mir::Type::String,
            kind: mir::OperandKind::Copy(place),
        }
    }

    /// Converts a validated, interpolation-eligible value to a `string`.
    /// Signed widths are widened to `long` and unsigned widths to `ulong`
    /// before the call so the runtime only needs one conversion routine per
    /// signedness, regardless of the source width.
    fn stringify(&mut self, operand: mir::Operand) -> mir::Operand {
        let intrinsic = match operand.type_ {
            mir::Type::String => return operand,
            mir::Type::Bool => mir::Intrinsic::StringFromBool,
            mir::Type::Char => mir::Intrinsic::StringFromChar,
            mir::Type::SByte | mir::Type::Short | mir::Type::Int | mir::Type::Long => {
                mir::Intrinsic::StringFromLong
            }
            mir::Type::Byte | mir::Type::UShort | mir::Type::UInt | mir::Type::ULong => {
                mir::Intrinsic::StringFromULong
            }
            mir::Type::Float => mir::Intrinsic::StringFromFloat,
            mir::Type::Double => mir::Intrinsic::StringFromDouble,
            _ => unreachable!("semantic analysis rejects types without a textual conversion"),
        };
        let destination = self.new_temporary(hir::Type::String);
        let place = mir::Place::Local(destination);
        self.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(place.clone()),
            intrinsic,
            arguments: vec![operand],
            return_type: mir::Type::String,
        });
        mir::Operand {
            type_: mir::Type::String,
            kind: mir::OperandKind::Copy(place),
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
        argument_order: &[usize],
    ) -> mir::Operand {
        let local = self.new_temporary(type_.clone());
        let place = mir::Place::Local(local);
        self.instruction(mir::Instruction::AllocateObject {
            destination: place.clone(),
            class,
            region: mir::AllocationRegion::Persistent,
        });
        let receiver = mir::Operand {
            type_: type_.clone(),
            kind: mir::OperandKind::Copy(place.clone()),
        };
        let mut lowered = vec![receiver.clone()];
        lowered.extend(self.lower_ordered_arguments(arguments, argument_order));
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
            initialization: mir::ArrayInitialization::Explicit,
            region: mir::AllocationRegion::Persistent,
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
                    bounds: mir::ArrayBounds::Checked,
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
        initialization: hir::NewArrayInitialization,
    ) -> mir::Operand {
        let local = self.new_temporary(type_.clone());
        let length = self
            .lower_expression(length)
            .expect("validated array length produces a value");
        self.instruction(mir::Instruction::AllocateArray {
            destination: mir::Place::Local(local),
            element_type: element_type.clone(),
            length,
            initialization: match initialization {
                hir::NewArrayInitialization::Default => mir::ArrayInitialization::Default,
                hir::NewArrayInitialization::Empty => mir::ArrayInitialization::Empty,
            },
            region: mir::AllocationRegion::Persistent,
        });
        mir::Operand {
            type_: type_.clone(),
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
        }
    }

    fn lower_new_list(&mut self, type_: &hir::Type, element_type: &hir::Type) -> mir::Operand {
        let local = self.new_temporary(type_.clone());
        // Always lowered `Persistent`; escape analysis (a later, whole-module
        // pass) rewrites this to `Temporary` when provably safe, exactly like
        // `AllocateObject`/`AllocateArray` above.
        self.instruction(mir::Instruction::AllocateList {
            destination: mir::Place::Local(local),
            element_type: element_type.clone(),
            region: mir::AllocationRegion::Persistent,
        });
        mir::Operand {
            type_: type_.clone(),
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
        }
    }

    fn lower_new_dictionary(
        &mut self,
        type_: &hir::Type,
        key_type: &hir::Type,
        value_type: &hir::Type,
    ) -> mir::Operand {
        let destination = self.new_temporary(type_.clone());
        self.instruction(mir::Instruction::AllocateDictionary {
            destination: mir::Place::Local(destination),
            key_type: key_type.clone(),
            value_type: value_type.clone(),
            region: mir::AllocationRegion::Persistent,
        });
        mir::Operand {
            type_: type_.clone(),
            kind: mir::OperandKind::Copy(mir::Place::Local(destination)),
        }
    }

    fn lower_new_string_builder(
        &mut self,
        type_: &hir::Type,
        class: hir::SymbolId,
    ) -> mir::Operand {
        let destination = self.new_temporary(type_.clone());
        self.instruction(mir::Instruction::AllocateStringBuilder {
            destination: mir::Place::Local(destination),
            class,
            region: mir::AllocationRegion::Persistent,
        });
        mir::Operand {
            type_: type_.clone(),
            kind: mir::OperandKind::Copy(mir::Place::Local(destination)),
        }
    }

    pub(super) fn lower_list_get(
        &mut self,
        list: mir::Operand,
        index: mir::Operand,
        element_type: &hir::Type,
    ) -> mir::Operand {
        let local = self.new_temporary(element_type.clone());
        let destination = mir::Place::Local(local);
        self.instruction(mir::Instruction::ListGet {
            destination: destination.clone(),
            list,
            index,
            element_type: element_type.clone(),
        });
        mir::Operand {
            type_: element_type.clone(),
            kind: mir::OperandKind::Copy(destination),
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
                hir::RuntimeErrorKind::AssertionTrue => mir::RuntimeErrorKind::AssertionTrue,
                hir::RuntimeErrorKind::AssertionFalse => mir::RuntimeErrorKind::AssertionFalse,
                hir::RuntimeErrorKind::AssertionEqual => mir::RuntimeErrorKind::AssertionEqual,
                hir::RuntimeErrorKind::MathSignNaN => mir::RuntimeErrorKind::MathSignNaN,
                hir::RuntimeErrorKind::CollectionRange => mir::RuntimeErrorKind::CollectionRange,
                hir::RuntimeErrorKind::RandomInvalidRange => {
                    mir::RuntimeErrorKind::RandomInvalidRange
                }
            })
        }
        hir::Intrinsic::AssertionEqual => mir::Intrinsic::AssertionEqual,
        hir::Intrinsic::ConsoleWrite => mir::Intrinsic::ConsoleWrite,
        hir::Intrinsic::ConsoleWriteLine => mir::Intrinsic::ConsoleWriteLine,
        hir::Intrinsic::ConsoleReadLine => mir::Intrinsic::ConsoleReadLine,
        // `FileIoResultLayout` is the identical type re-exported by both
        // crates (`aster_mir::FileIoResultLayout` is `aster_hir`'s), so the
        // symbols HIR lowering resolved pass straight through unchanged.
        hir::Intrinsic::FileReadAllText(layout) => mir::Intrinsic::FileReadAllText(layout),
        hir::Intrinsic::FileWriteAllText(layout) => mir::Intrinsic::FileWriteAllText(layout),
        hir::Intrinsic::FileAppendAllText(layout) => mir::Intrinsic::FileAppendAllText(layout),
        hir::Intrinsic::FileListFiles(layout) => mir::Intrinsic::FileListFiles(layout),
        hir::Intrinsic::FileListDirectories(layout) => mir::Intrinsic::FileListDirectories(layout),
        hir::Intrinsic::FileExists(layout) => mir::Intrinsic::FileExists(layout),
        hir::Intrinsic::DirectoryExists(layout) => mir::Intrinsic::DirectoryExists(layout),
        hir::Intrinsic::FileCreateDirectory(layout) => mir::Intrinsic::FileCreateDirectory(layout),
        hir::Intrinsic::FileDeleteFile(layout) => mir::Intrinsic::FileDeleteFile(layout),
        hir::Intrinsic::FileDeleteDirectory(layout) => mir::Intrinsic::FileDeleteDirectory(layout),
        hir::Intrinsic::StringTrim => mir::Intrinsic::StringTrim,
        hir::Intrinsic::StringLastIndexOf => mir::Intrinsic::StringLastIndexOf,
        hir::Intrinsic::StringTrimStart => mir::Intrinsic::StringTrimStart,
        hir::Intrinsic::StringTrimEnd => mir::Intrinsic::StringTrimEnd,
        hir::Intrinsic::StringJoinArray => mir::Intrinsic::StringJoinArray,
        hir::Intrinsic::StringConcatArray => mir::Intrinsic::StringConcatArray,
        hir::Intrinsic::StringRepeat => mir::Intrinsic::StringRepeat,
        hir::Intrinsic::StringToChars => mir::Intrinsic::StringToChars,
        hir::Intrinsic::StringFromChars => mir::Intrinsic::StringFromChars,
        hir::Intrinsic::StringReplace => mir::Intrinsic::StringReplace,
        hir::Intrinsic::StringSplit => mir::Intrinsic::StringSplit,
        hir::Intrinsic::MathUnaryFloat => mir::Intrinsic::MathUnaryFloat,
        hir::Intrinsic::MathUnaryDouble => mir::Intrinsic::MathUnaryDouble,
        hir::Intrinsic::MathBinaryFloat => mir::Intrinsic::MathBinaryFloat,
        hir::Intrinsic::MathBinaryDouble => mir::Intrinsic::MathBinaryDouble,
        hir::Intrinsic::MathPredicateFloat => mir::Intrinsic::MathPredicateFloat,
        hir::Intrinsic::MathPredicateDouble => mir::Intrinsic::MathPredicateDouble,
        hir::Intrinsic::MathPowFloat => mir::Intrinsic::MathPowFloat,
        hir::Intrinsic::MathPowDouble => mir::Intrinsic::MathPowDouble,
        hir::Intrinsic::TimeMonotonicMilliseconds => mir::Intrinsic::TimeMonotonicMilliseconds,
        hir::Intrinsic::TimeUnixMilliseconds => mir::Intrinsic::TimeUnixMilliseconds,
        hir::Intrinsic::RandomMix => mir::Intrinsic::RandomMix,
        hir::Intrinsic::StringBuilderLength => mir::Intrinsic::StringBuilderLength,
        hir::Intrinsic::StringBuilderClear => mir::Intrinsic::StringBuilderClear,
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

/// Collect a string-add tree only when every leaf is already a stable string
/// value. Calls and other expressions retain pairwise concatenation so a
/// failing intermediate allocation cannot move past a later side effect or
/// controlled error.
fn collect_static_string_concat<'a>(
    expression: &'a hir::Expression,
    parts: &mut Vec<&'a hir::Expression>,
) -> bool {
    let mut pending = vec![expression];
    while let Some(expression) = pending.pop() {
        match &expression.kind {
            hir::ExpressionKind::Binary {
                left,
                operator: hir::BinaryOperator::Add,
                right,
            } if expression.type_ == hir::Type::String => {
                pending.push(right);
                pending.push(left);
            }
            hir::ExpressionKind::Literal(hir::Literal::String(_))
            | hir::ExpressionKind::Symbol(_)
                if expression.type_ == hir::Type::String =>
            {
                parts.push(expression);
            }
            _ => {
                parts.clear();
                return false;
            }
        }
    }
    true
}

fn is_string_concat(expression: &hir::Expression) -> bool {
    matches!(
        &expression.kind,
        hir::ExpressionKind::Binary {
            operator: hir::BinaryOperator::Add,
            ..
        }
    ) && expression.type_ == hir::Type::String
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
