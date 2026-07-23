use super::expressions::{boolean_operand, one_constant};
use super::{FunctionLowerer, LoopTargets, hir, mir};

impl FunctionLowerer {
    pub(super) fn lower_block(&mut self, block: &hir::Block) {
        for statement in &block.statements {
            if self.current.is_none() {
                break;
            }
            self.lower_statement(statement);
        }
    }

    pub(super) fn lower_statement(&mut self, statement: &hir::Statement) {
        match statement {
            hir::Statement::Variable(variable) => self.lower_variable(variable),
            hir::Statement::Return(value) => {
                let value = value
                    .as_ref()
                    .and_then(|value| self.lower_expression(value));
                // Inside a generated async `MoveNext`, a source-level `return`
                // publishes the value as the async task's candidate result and
                // then returns the machine's `Completed` status, never the
                // value itself. See `async_machine`.
                if self.async_handle.is_some() {
                    self.emit_async_return(value);
                } else {
                    self.terminate_current(mir::Terminator::Return(value));
                }
            }
            hir::Statement::Expression(expression) => {
                self.lower_expression(expression);
            }
            hir::Statement::If {
                condition,
                then_block,
                else_block,
            } => self.lower_if(condition, then_block, else_block.as_ref()),
            hir::Statement::While { condition, body } => self.lower_while(condition, body),
            hir::Statement::For {
                initializer,
                condition,
                update,
                body,
            } => self.lower_for(
                initializer.as_deref(),
                condition.as_ref(),
                update.as_ref(),
                body,
            ),
            hir::Statement::ForEach {
                element,
                collection,
                body,
            } => self.lower_foreach(element, collection, body),
            hir::Statement::Switch {
                value,
                cases,
                default,
            } => self.lower_switch(value, cases, default.as_ref()),
            hir::Statement::Break => {
                let target = self
                    .loops
                    .last()
                    .expect("validated break has a loop")
                    .break_block;
                self.terminate_current(mir::Terminator::Goto(target));
            }
            hir::Statement::Continue => {
                let target = self
                    .loops
                    .last()
                    .expect("validated continue has a loop")
                    .continue_block;
                self.terminate_current(mir::Terminator::Goto(target));
            }
        }
    }

    fn lower_variable(&mut self, variable: &hir::Variable) {
        let local = self.source_local(
            variable.symbol,
            variable.name.clone(),
            variable.type_.clone(),
            variable.mutable,
        );
        self.locals.push(local.clone());
        if let Some(initializer) = &variable.initializer
            && let Some(value) = self.lower_expression(initializer)
        {
            self.assign(
                mir::Place::Local(local.id),
                mir::Rvalue {
                    type_: variable.type_.clone(),
                    kind: mir::RvalueKind::Use(value),
                },
            );
        }
    }

    fn lower_if(
        &mut self,
        condition: &hir::Expression,
        then_block: &hir::Block,
        else_block: Option<&hir::Block>,
    ) {
        let condition = self
            .lower_expression(condition)
            .expect("validated condition produces a value");
        let then_id = self.new_block();
        if let Some(else_block) = else_block {
            let else_id = self.new_block();
            self.terminate_current(mir::Terminator::Branch {
                condition,
                then_block: then_id,
                else_block: else_id,
            });

            self.current = Some(then_id);
            self.lower_block(then_block);
            let then_end = self.current.take();

            self.current = Some(else_id);
            self.lower_block(else_block);
            let else_end = self.current.take();

            if then_end.is_some() || else_end.is_some() {
                let join = self.new_block();
                if let Some(block) = then_end {
                    self.terminate(block, mir::Terminator::Goto(join));
                }
                if let Some(block) = else_end {
                    self.terminate(block, mir::Terminator::Goto(join));
                }
                self.current = Some(join);
            }
        } else {
            let join = self.new_block();
            self.terminate_current(mir::Terminator::Branch {
                condition,
                then_block: then_id,
                else_block: join,
            });
            self.current = Some(then_id);
            self.lower_block(then_block);
            if let Some(block) = self.current.take() {
                self.terminate(block, mir::Terminator::Goto(join));
            }
            self.current = Some(join);
        }
    }

    fn lower_while(&mut self, condition: &hir::Expression, body: &hir::Block) {
        let condition_id = self.new_block();
        let body_id = self.new_block();
        let exit_id = self.new_block();
        self.terminate_current(mir::Terminator::Goto(condition_id));

        self.current = Some(condition_id);
        let condition = self
            .lower_expression(condition)
            .expect("validated condition produces a value");
        self.terminate_current(mir::Terminator::Branch {
            condition,
            then_block: body_id,
            else_block: exit_id,
        });

        self.loops.push(LoopTargets {
            break_block: exit_id,
            continue_block: condition_id,
        });
        self.current = Some(body_id);
        self.lower_block(body);
        if let Some(block) = self.current.take() {
            self.terminate(block, mir::Terminator::Goto(condition_id));
        }
        self.loops.pop();
        self.current = Some(exit_id);
    }

    fn lower_for(
        &mut self,
        initializer: Option<&hir::Statement>,
        condition: Option<&hir::Expression>,
        update: Option<&hir::Expression>,
        body: &hir::Block,
    ) {
        if let Some(initializer) = initializer {
            self.lower_statement(initializer);
        }
        let condition_id = self.new_block();
        let body_id = self.new_block();
        let update_id = self.new_block();
        let exit_id = self.new_block();
        self.terminate_current(mir::Terminator::Goto(condition_id));

        self.current = Some(condition_id);
        let condition = condition.map_or_else(
            || boolean_operand(true),
            |condition| {
                self.lower_expression(condition)
                    .expect("validated condition produces a value")
            },
        );
        self.terminate_current(mir::Terminator::Branch {
            condition,
            then_block: body_id,
            else_block: exit_id,
        });

        self.loops.push(LoopTargets {
            break_block: exit_id,
            continue_block: update_id,
        });
        self.current = Some(body_id);
        self.lower_block(body);
        if let Some(block) = self.current.take() {
            self.terminate(block, mir::Terminator::Goto(update_id));
        }
        self.loops.pop();

        self.current = Some(update_id);
        if let Some(update) = update {
            self.lower_expression(update);
        }
        if let Some(block) = self.current.take() {
            self.terminate(block, mir::Terminator::Goto(condition_id));
        }
        self.current = Some(exit_id);
    }

    /// Expands the compiler-known array foreach to the ordinary indexed CFG.
    /// Collection and length are materialized once before the first branch.
    #[allow(clippy::too_many_lines)]
    fn lower_foreach(
        &mut self,
        element: &hir::Variable,
        collection: &hir::Expression,
        body: &hir::Block,
    ) {
        let collection_operand = self
            .lower_expression(collection)
            .expect("validated foreach collection produces a value");
        let collection_local = self.new_temporary(collection.type_.clone());
        self.assign(
            mir::Place::Local(collection_local),
            mir::Rvalue {
                type_: collection.type_.clone(),
                kind: mir::RvalueKind::Use(collection_operand),
            },
        );
        let collection_operand = mir::Operand {
            type_: collection.type_.clone(),
            kind: mir::OperandKind::Copy(mir::Place::Local(collection_local)),
        };
        let length_local = self.new_temporary(hir::Type::Int);
        self.assign(
            mir::Place::Local(length_local),
            mir::Rvalue {
                type_: hir::Type::Int,
                kind: mir::RvalueKind::ArrayLength(collection_operand.clone()),
            },
        );
        let index_local = self.new_temporary(hir::Type::Int);
        self.assign(
            mir::Place::Local(index_local),
            mir::Rvalue {
                type_: hir::Type::Int,
                kind: mir::RvalueKind::Use(mir::Operand {
                    type_: hir::Type::Int,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer("0".to_owned())),
                }),
            },
        );
        let element_local = self.source_local(
            element.symbol,
            element.name.clone(),
            element.type_.clone(),
            false,
        );
        self.locals.push(element_local.clone());

        let condition_id = self.new_block();
        let body_id = self.new_block();
        let update_id = self.new_block();
        let exit_id = self.new_block();
        self.terminate_current(mir::Terminator::Goto(condition_id));

        self.current = Some(condition_id);
        let condition_local = self.new_temporary(hir::Type::Bool);
        self.assign(
            mir::Place::Local(condition_local),
            mir::Rvalue {
                type_: hir::Type::Bool,
                kind: mir::RvalueKind::Binary {
                    left: mir::Operand {
                        type_: hir::Type::Int,
                        kind: mir::OperandKind::Copy(mir::Place::Local(index_local)),
                    },
                    operator: mir::BinaryOperator::Less,
                    right: mir::Operand {
                        type_: hir::Type::Int,
                        kind: mir::OperandKind::Copy(mir::Place::Local(length_local)),
                    },
                },
            },
        );
        self.terminate_current(mir::Terminator::Branch {
            condition: mir::Operand {
                type_: hir::Type::Bool,
                kind: mir::OperandKind::Copy(mir::Place::Local(condition_local)),
            },
            then_block: body_id,
            else_block: exit_id,
        });

        self.loops.push(LoopTargets {
            break_block: exit_id,
            continue_block: update_id,
        });
        self.current = Some(body_id);
        self.assign(
            mir::Place::Local(element_local.id),
            mir::Rvalue {
                type_: element.type_.clone(),
                kind: mir::RvalueKind::Use(mir::Operand {
                    type_: element.type_.clone(),
                    kind: mir::OperandKind::Copy(mir::Place::Index {
                        array: Box::new(collection_operand.clone()),
                        index: Box::new(mir::Operand {
                            type_: hir::Type::Int,
                            kind: mir::OperandKind::Copy(mir::Place::Local(index_local)),
                        }),
                        element_type: element.type_.clone(),
                    }),
                }),
            },
        );
        self.lower_block(body);
        if let Some(block) = self.current.take() {
            self.terminate(block, mir::Terminator::Goto(update_id));
        }
        self.loops.pop();

        self.current = Some(update_id);
        self.assign(
            mir::Place::Local(index_local),
            mir::Rvalue {
                type_: hir::Type::Int,
                kind: mir::RvalueKind::Binary {
                    left: mir::Operand {
                        type_: hir::Type::Int,
                        kind: mir::OperandKind::Copy(mir::Place::Local(index_local)),
                    },
                    operator: mir::BinaryOperator::Add,
                    right: mir::Operand {
                        type_: hir::Type::Int,
                        kind: mir::OperandKind::Constant(one_constant(&hir::Type::Int)),
                    },
                },
            },
        );
        self.terminate_current(mir::Terminator::Goto(condition_id));
        self.current = Some(exit_id);
    }

    fn lower_switch(
        &mut self,
        value: &hir::Expression,
        cases: &[hir::SwitchCase],
        default: Option<&hir::Block>,
    ) {
        let value_operand = self
            .lower_expression(value)
            .expect("validated switch value produces a value");
        let value_local = self.new_temporary(value.type_.clone());
        self.assign(
            mir::Place::Local(value_local),
            mir::Rvalue {
                type_: value.type_.clone(),
                kind: mir::RvalueKind::Use(value_operand),
            },
        );
        let enum_operand = mir::Operand {
            type_: value.type_.clone(),
            kind: mir::OperandKind::Copy(mir::Place::Local(value_local)),
        };
        let discriminant =
            self.temporary(mir::Type::UInt, mir::RvalueKind::Discriminant(enum_operand));
        let join = self.new_block();
        let mut continues = false;
        for case in cases {
            let arm = self.new_block();
            let next = self.new_block();
            let condition = self.temporary(
                mir::Type::Bool,
                mir::RvalueKind::Binary {
                    left: discriminant.clone(),
                    operator: mir::BinaryOperator::Equal,
                    right: mir::Operand {
                        type_: mir::Type::UInt,
                        kind: mir::OperandKind::Constant(mir::Constant::Integer(
                            case.tag.to_string(),
                        )),
                    },
                },
            );
            self.terminate_current(mir::Terminator::Branch {
                condition,
                then_block: arm,
                else_block: next,
            });
            self.current = Some(arm);
            let definition = self
                .enums
                .values()
                .flat_map(|value| &value.cases)
                .find(|value| value.symbol == case.case)
                .expect("resolved switch case exists")
                .clone();
            for (binding, field) in case.bindings.iter().zip(&definition.fields) {
                let local = self.source_local(
                    binding.symbol,
                    binding.name.clone(),
                    binding.type_.clone(),
                    true,
                );
                self.locals.push(local.clone());
                self.assign(
                    mir::Place::Local(local.id),
                    mir::Rvalue {
                        type_: field.type_.clone(),
                        kind: mir::RvalueKind::Use(mir::Operand {
                            type_: field.type_.clone(),
                            kind: mir::OperandKind::Copy(mir::Place::EnumField {
                                base: Box::new(mir::Place::Local(value_local)),
                                case: case.case,
                                field: field.symbol,
                            }),
                        }),
                    },
                );
            }
            self.lower_block(&case.body);
            if let Some(block) = self.current.take() {
                self.terminate(block, mir::Terminator::Goto(join));
                continues = true;
            }
            self.current = Some(next);
        }
        if let Some(default) = default {
            self.lower_block(default);
        }
        if let Some(block) = self.current.take() {
            if default.is_some() {
                self.terminate(block, mir::Terminator::Goto(join));
                continues = true;
            } else {
                self.terminate(block, mir::Terminator::Unreachable);
            }
        }
        if continues {
            self.current = Some(join);
        } else {
            self.terminate(join, mir::Terminator::Unreachable);
            self.current = None;
        }
    }

    /// Lower postfix `?` into explicit control flow, reusing the enum tag,
    /// payload, construction, and return machinery. The operand is evaluated
    /// once; the `Error` branch early-returns and never joins, and the `Ok`
    /// branch becomes the live block so the surrounding expression continues.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_propagate_result(
        &mut self,
        operand: &hir::Expression,
        success_type: &hir::Type,
        error_type: &hir::Type,
        ok_case: hir::SymbolId,
        ok_field: hir::SymbolId,
        error_case: hir::SymbolId,
        error_field: hir::SymbolId,
        error_tag: u32,
        return_type: &hir::Type,
        return_error_case: hir::SymbolId,
        return_error_field: hir::SymbolId,
        return_error_tag: u32,
    ) -> mir::Operand {
        let value_operand = self
            .lower_expression(operand)
            .expect("validated `?` operand produces a value");
        let value_local = self.new_temporary(operand.type_.clone());
        self.assign(
            mir::Place::Local(value_local),
            mir::Rvalue {
                type_: operand.type_.clone(),
                kind: mir::RvalueKind::Use(value_operand),
            },
        );
        let discriminant = self.temporary(
            mir::Type::UInt,
            mir::RvalueKind::Discriminant(mir::Operand {
                type_: operand.type_.clone(),
                kind: mir::OperandKind::Copy(mir::Place::Local(value_local)),
            }),
        );
        let is_error = self.temporary(
            mir::Type::Bool,
            mir::RvalueKind::Binary {
                left: discriminant,
                operator: mir::BinaryOperator::Equal,
                right: mir::Operand {
                    type_: mir::Type::UInt,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer(error_tag.to_string())),
                },
            },
        );
        let error_block = self.new_block();
        let ok_block = self.new_block();
        self.terminate_current(mir::Terminator::Branch {
            condition: is_error,
            then_block: error_block,
            else_block: ok_block,
        });
        self.current = Some(error_block);
        let error_value = self.enum_field(value_local, error_case, error_field, error_type);
        let propagated = self.temporary(
            return_type.clone(),
            mir::RvalueKind::EnumConstruct {
                case: return_error_case,
                tag: return_error_tag,
                fields: vec![mir::FieldOperand {
                    field: return_error_field,
                    value: error_value,
                }],
            },
        );
        self.terminate_current(mir::Terminator::Return(Some(propagated)));
        self.current = Some(ok_block);
        self.enum_field(value_local, ok_case, ok_field, success_type)
    }

    /// Postfix `?` on the official `aster.core.Option<T>`: the same
    /// evaluate-once, inspect-tag, branch-and-return shape as
    /// [`Self::lower_propagate_result`], except `None` carries no payload to
    /// extract -- the early return is a zero-field `EnumConstruct` for the
    /// enclosing function's own `Option<U>.None`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn lower_propagate_option(
        &mut self,
        operand: &hir::Expression,
        success_type: &hir::Type,
        some_case: hir::SymbolId,
        some_field: hir::SymbolId,
        none_tag: u32,
        return_type: &hir::Type,
        return_none_case: hir::SymbolId,
        return_none_tag: u32,
    ) -> mir::Operand {
        let value_operand = self
            .lower_expression(operand)
            .expect("validated `?` operand produces a value");
        let value_local = self.new_temporary(operand.type_.clone());
        self.assign(
            mir::Place::Local(value_local),
            mir::Rvalue {
                type_: operand.type_.clone(),
                kind: mir::RvalueKind::Use(value_operand),
            },
        );
        let discriminant = self.temporary(
            mir::Type::UInt,
            mir::RvalueKind::Discriminant(mir::Operand {
                type_: operand.type_.clone(),
                kind: mir::OperandKind::Copy(mir::Place::Local(value_local)),
            }),
        );
        let is_none = self.temporary(
            mir::Type::Bool,
            mir::RvalueKind::Binary {
                left: discriminant,
                operator: mir::BinaryOperator::Equal,
                right: mir::Operand {
                    type_: mir::Type::UInt,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer(none_tag.to_string())),
                },
            },
        );
        let none_block = self.new_block();
        let some_block = self.new_block();
        self.terminate_current(mir::Terminator::Branch {
            condition: is_none,
            then_block: none_block,
            else_block: some_block,
        });
        self.current = Some(none_block);
        let propagated = self.temporary(
            return_type.clone(),
            mir::RvalueKind::EnumConstruct {
                case: return_none_case,
                tag: return_none_tag,
                fields: vec![],
            },
        );
        self.terminate_current(mir::Terminator::Return(Some(propagated)));
        self.current = Some(some_block);
        self.enum_field(value_local, some_case, some_field, success_type)
    }

    fn enum_field(
        &mut self,
        base: mir::LocalId,
        case: hir::SymbolId,
        field: hir::SymbolId,
        type_: &hir::Type,
    ) -> mir::Operand {
        self.temporary(
            type_.clone(),
            mir::RvalueKind::Use(mir::Operand {
                type_: type_.clone(),
                kind: mir::OperandKind::Copy(mir::Place::EnumField {
                    base: Box::new(mir::Place::Local(base)),
                    case,
                    field,
                }),
            }),
        )
    }
}
