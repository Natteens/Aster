use super::expressions::{expression_symbol, lower_intrinsic};
use super::{FunctionLowerer, hir, mir};

impl FunctionLowerer {
    pub(super) fn lower_call(
        &mut self,
        callee: &hir::Expression,
        arguments: &[hir::Expression],
        argument_order: &[usize],
        return_type: &hir::Type,
    ) -> Option<mir::Operand> {
        let function = expression_symbol(callee).expect("validated call has a resolved symbol");
        if let Some(intrinsic) = self.intrinsics.get(&function).copied() {
            return self.lower_intrinsic_call(intrinsic, arguments, return_type);
        }
        if let hir::ExpressionKind::Member { object, .. } = &callee.kind
            && matches!(object.type_, hir::Type::Interface(_))
        {
            let receiver = self
                .lower_expression(object)
                .expect("interface method receiver produces a value");
            let lowered_arguments = self.lower_ordered_arguments(arguments, argument_order);
            if return_type == &hir::Type::Void {
                self.instruction(mir::Instruction::CallInterface {
                    destination: None,
                    receiver,
                    method: function,
                    arguments: lowered_arguments,
                    return_type: mir::Type::Void,
                });
                return None;
            }
            let local = self.new_temporary(return_type.clone());
            let destination = mir::Place::Local(local);
            self.instruction(mir::Instruction::CallInterface {
                destination: Some(destination.clone()),
                receiver,
                method: function,
                arguments: lowered_arguments,
                return_type: return_type.clone(),
            });
            return Some(mir::Operand {
                type_: return_type.clone(),
                kind: mir::OperandKind::Copy(destination),
            });
        }
        let mut lowered_arguments = Vec::new();
        if let hir::ExpressionKind::Member { object, .. } = &callee.kind {
            lowered_arguments.push(
                self.lower_expression(object)
                    .expect("method receiver produces a value"),
            );
        }
        lowered_arguments.extend(self.lower_ordered_arguments(arguments, argument_order));
        if return_type == &hir::Type::Void {
            self.instruction(mir::Instruction::Call {
                destination: None,
                function,
                arguments: lowered_arguments,
                return_type: mir::Type::Void,
            });
            None
        } else {
            let local = self.new_temporary(return_type.clone());
            let destination = mir::Place::Local(local);
            self.instruction(mir::Instruction::Call {
                destination: Some(destination.clone()),
                function,
                arguments: lowered_arguments,
                return_type: return_type.clone(),
            });
            Some(mir::Operand {
                type_: return_type.clone(),
                kind: mir::OperandKind::Copy(destination),
            })
        }
    }

    pub(super) fn lower_ordered_arguments(
        &mut self,
        arguments: &[hir::Expression],
        argument_order: &[usize],
    ) -> Vec<mir::Operand> {
        debug_assert_eq!(arguments.len(), argument_order.len());
        let mut ordered = vec![None; arguments.len()];
        for (argument, parameter) in arguments.iter().zip(argument_order) {
            let value = self
                .lower_expression(argument)
                .expect("validated call argument produces a value");
            assert!(
                *parameter < ordered.len() && ordered[*parameter].replace(value).is_none(),
                "validated call argument mapping is a parameter permutation"
            );
        }
        ordered
            .into_iter()
            .map(|argument| argument.expect("every required parameter has an argument"))
            .collect()
    }

    /// Lowers a call to a function nominally bound to a host [`hir::Intrinsic`]
    /// (`hir::Function::intrinsic`, resolved by `SymbolId`, never by name at
    /// this layer). Each arm's shape matches the official declaration the
    /// intrinsic is bound to: the nullary, `void`-returning math error
    /// reporters; `aster.io.Write`/`WriteLine`, one `string` argument,
    /// `void`; `aster.io.ReadLine`, no arguments, an `Option<string>`
    /// destination whose region always starts `Persistent` and is narrowed
    /// by escape analysis exactly like `StringFrom*`/`Substring`.
    #[allow(clippy::too_many_lines)]
    fn lower_intrinsic_call(
        &mut self,
        intrinsic: hir::Intrinsic,
        arguments: &[hir::Expression],
        return_type: &hir::Type,
    ) -> Option<mir::Operand> {
        match intrinsic {
            hir::Intrinsic::ReportRuntimeError(_) => {
                debug_assert!(
                    arguments.is_empty(),
                    "validated runtime-error intrinsic is nullary"
                );
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic: lower_intrinsic(intrinsic),
                    arguments: Vec::new(),
                    return_type: mir::Type::Void,
                });
                None
            }
            hir::Intrinsic::AssertionEqual => {
                let expected = self
                    .lower_expression(&arguments[0])
                    .expect("validated assertion expected value produces a string");
                let actual = self
                    .lower_expression(&arguments[1])
                    .expect("validated assertion actual value produces a string");
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic: lower_intrinsic(intrinsic),
                    arguments: vec![expected, actual],
                    return_type: mir::Type::Void,
                });
                None
            }
            hir::Intrinsic::ConsoleWrite | hir::Intrinsic::ConsoleWriteLine => {
                let value = self
                    .lower_expression(&arguments[0])
                    .expect("validated console write argument produces a value");
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: None,
                    intrinsic: lower_intrinsic(intrinsic),
                    arguments: vec![value],
                    return_type: mir::Type::Void,
                });
                None
            }
            hir::Intrinsic::ConsoleReadLine => {
                let local = self.new_temporary(return_type.clone());
                let destination = mir::Place::Local(local);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::ConsoleReadLine,
                    arguments: Vec::new(),
                    return_type: return_type.clone(),
                });
                Some(mir::Operand {
                    type_: return_type.clone(),
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::Intrinsic::StringTrim
            | hir::Intrinsic::StringLastIndexOf
            | hir::Intrinsic::StringTrimStart
            | hir::Intrinsic::StringTrimEnd
            | hir::Intrinsic::StringJoinArray
            | hir::Intrinsic::StringConcatArray
            | hir::Intrinsic::StringRepeat
            | hir::Intrinsic::StringToChars
            | hir::Intrinsic::StringFromChars
            | hir::Intrinsic::StringReplace
            | hir::Intrinsic::StringSplit
            | hir::Intrinsic::MathUnaryFloat
            | hir::Intrinsic::MathUnaryDouble
            | hir::Intrinsic::MathBinaryFloat
            | hir::Intrinsic::MathBinaryDouble
            | hir::Intrinsic::MathPredicateFloat
            | hir::Intrinsic::MathPredicateDouble
            | hir::Intrinsic::MathPowFloat
            | hir::Intrinsic::MathPowDouble
            | hir::Intrinsic::TimeMonotonicMilliseconds
            | hir::Intrinsic::TimeUnixMilliseconds
            | hir::Intrinsic::RandomMix
            | hir::Intrinsic::StringBuilderLength
            | hir::Intrinsic::StringBuilderClear => {
                let arguments = arguments
                    .iter()
                    .map(|argument| {
                        self.lower_expression(argument)
                            .expect("validated intrinsic argument produces a value")
                    })
                    .collect();
                let local = self.new_temporary(return_type.clone());
                let destination = mir::Place::Local(local);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: lower_intrinsic(intrinsic),
                    arguments,
                    return_type: return_type.clone(),
                });
                Some(mir::Operand {
                    type_: return_type.clone(),
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::Intrinsic::FileReadAllText(layout) => {
                let path = self
                    .lower_expression(&arguments[0])
                    .expect("validated ReadAllText path argument produces a value");
                let local = self.new_temporary(return_type.clone());
                let destination = mir::Place::Local(local);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::FileReadAllText(layout),
                    arguments: vec![path],
                    return_type: return_type.clone(),
                });
                Some(mir::Operand {
                    type_: return_type.clone(),
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::Intrinsic::FileWriteAllText(layout) => {
                let path = self
                    .lower_expression(&arguments[0])
                    .expect("validated WriteAllText path argument produces a value");
                let content = self
                    .lower_expression(&arguments[1])
                    .expect("validated WriteAllText content argument produces a value");
                let local = self.new_temporary(return_type.clone());
                let destination = mir::Place::Local(local);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::FileWriteAllText(layout),
                    arguments: vec![path, content],
                    return_type: return_type.clone(),
                });
                Some(mir::Operand {
                    type_: return_type.clone(),
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::Intrinsic::FileAppendAllText(layout) => {
                let path = self
                    .lower_expression(&arguments[0])
                    .expect("validated append path");
                let content = self
                    .lower_expression(&arguments[1])
                    .expect("validated append content");
                let destination = mir::Place::Local(self.new_temporary(return_type.clone()));
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::FileAppendAllText(layout),
                    arguments: vec![path, content],
                    return_type: return_type.clone(),
                });
                Some(mir::Operand {
                    type_: return_type.clone(),
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::Intrinsic::FileListFiles(layout) => {
                let directory = self
                    .lower_expression(&arguments[0])
                    .expect("validated ListFiles directory argument produces a value");
                let local = self.new_temporary(return_type.clone());
                let destination = mir::Place::Local(local);
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::FileListFiles(layout),
                    arguments: vec![directory],
                    return_type: return_type.clone(),
                });
                Some(mir::Operand {
                    type_: return_type.clone(),
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::Intrinsic::FileListDirectories(layout) => {
                let directory = self
                    .lower_expression(&arguments[0])
                    .expect("validated directory path");
                let destination = mir::Place::Local(self.new_temporary(return_type.clone()));
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic: mir::Intrinsic::FileListDirectories(layout),
                    arguments: vec![directory],
                    return_type: return_type.clone(),
                });
                Some(mir::Operand {
                    type_: return_type.clone(),
                    kind: mir::OperandKind::Copy(destination),
                })
            }
            hir::Intrinsic::FileExists(layout)
            | hir::Intrinsic::DirectoryExists(layout)
            | hir::Intrinsic::FileCreateDirectory(layout)
            | hir::Intrinsic::FileDeleteFile(layout)
            | hir::Intrinsic::FileDeleteDirectory(layout) => {
                let path = self
                    .lower_expression(&arguments[0])
                    .expect("validated path");
                let intrinsic = match intrinsic {
                    hir::Intrinsic::FileExists(_) => mir::Intrinsic::FileExists(layout),
                    hir::Intrinsic::DirectoryExists(_) => mir::Intrinsic::DirectoryExists(layout),
                    hir::Intrinsic::FileCreateDirectory(_) => {
                        mir::Intrinsic::FileCreateDirectory(layout)
                    }
                    hir::Intrinsic::FileDeleteFile(_) => mir::Intrinsic::FileDeleteFile(layout),
                    hir::Intrinsic::FileDeleteDirectory(_) => {
                        mir::Intrinsic::FileDeleteDirectory(layout)
                    }
                    _ => unreachable!(),
                };
                let destination = mir::Place::Local(self.new_temporary(return_type.clone()));
                self.instruction(mir::Instruction::CallIntrinsic {
                    destination: Some(destination.clone()),
                    intrinsic,
                    arguments: vec![path],
                    return_type: return_type.clone(),
                });
                Some(mir::Operand {
                    type_: return_type.clone(),
                    kind: mir::OperandKind::Copy(destination),
                })
            }
        }
    }
}
