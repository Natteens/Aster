use super::expressions::{compound_operator, one_constant};
use super::{FunctionLowerer, hir, mir};

impl FunctionLowerer {
    pub(super) fn place_operand(&mut self, expression: &hir::Expression) -> mir::Operand {
        mir::Operand {
            type_: expression.type_.clone(),
            kind: mir::OperandKind::Copy(self.place(expression)),
        }
    }

    /// Prefix forms produce the updated value; postfix forms produce the value
    /// observed before the update.
    pub(super) fn lower_increment_decrement(
        &mut self,
        operator: hir::IncrementOperator,
        prefix: bool,
        target: &hir::Expression,
    ) -> mir::Operand {
        let place = self.place(target);
        let type_ = target.type_.clone();
        let old_value = if prefix {
            None
        } else {
            let old = self.new_temporary(type_.clone());
            self.assign(
                mir::Place::Local(old),
                mir::Rvalue {
                    type_: type_.clone(),
                    kind: mir::RvalueKind::Use(mir::Operand {
                        type_: type_.clone(),
                        kind: mir::OperandKind::Copy(place.clone()),
                    }),
                },
            );
            Some(old)
        };
        let step = match operator {
            hir::IncrementOperator::Increment => mir::BinaryOperator::Add,
            hir::IncrementOperator::Decrement => mir::BinaryOperator::Subtract,
        };
        self.assign(
            place.clone(),
            mir::Rvalue {
                type_: type_.clone(),
                kind: mir::RvalueKind::Binary {
                    left: mir::Operand {
                        type_: type_.clone(),
                        kind: mir::OperandKind::Copy(place.clone()),
                    },
                    operator: step,
                    right: mir::Operand {
                        type_: type_.clone(),
                        kind: mir::OperandKind::Constant(one_constant(&type_)),
                    },
                },
            },
        );
        let result = old_value.map_or(place, mir::Place::Local);
        mir::Operand {
            type_,
            kind: mir::OperandKind::Copy(result),
        }
    }

    pub(super) fn lower_assignment(
        &mut self,
        target: &hir::Expression,
        operator: hir::AssignmentOperator,
        value: &hir::Expression,
    ) -> mir::Operand {
        let place = self.place(target);
        if target.type_ == hir::Type::String && operator == hir::AssignmentOperator::AddAssign {
            let current = self.temporary(
                hir::Type::String,
                mir::RvalueKind::Use(mir::Operand {
                    type_: hir::Type::String,
                    kind: mir::OperandKind::Copy(place.clone()),
                }),
            );
            let value = self
                .lower_expression(value)
                .expect("validated assignment value produces a value");
            let concatenated = self.emit_string_concat(current, value);
            self.assign(
                place.clone(),
                mir::Rvalue {
                    type_: hir::Type::String,
                    kind: mir::RvalueKind::Use(concatenated),
                },
            );
            return mir::Operand {
                type_: hir::Type::String,
                kind: mir::OperandKind::Copy(place),
            };
        }
        let value = self
            .lower_expression(value)
            .expect("validated assignment value produces a value");
        let rvalue = if operator == hir::AssignmentOperator::Assign {
            mir::Rvalue {
                type_: target.type_.clone(),
                kind: mir::RvalueKind::Use(value),
            }
        } else {
            mir::Rvalue {
                type_: target.type_.clone(),
                kind: mir::RvalueKind::Binary {
                    left: mir::Operand {
                        type_: target.type_.clone(),
                        kind: mir::OperandKind::Copy(place.clone()),
                    },
                    operator: compound_operator(operator),
                    right: value,
                },
            }
        };
        self.assign(place.clone(), rvalue);
        mir::Operand {
            type_: target.type_.clone(),
            kind: mir::OperandKind::Copy(place),
        }
    }

    pub(super) fn lower_property_assignment(
        &mut self,
        object: &hir::Expression,
        getter: Option<hir::SymbolId>,
        setter: hir::SymbolId,
        operator: hir::AssignmentOperator,
        value: &hir::Expression,
        type_: &hir::Type,
    ) -> mir::Operand {
        let receiver = self
            .lower_expression(object)
            .expect("property receiver produces a value");
        let assigned = if operator == hir::AssignmentOperator::Assign {
            self.lower_expression(value)
                .expect("property assignment value produces a value")
        } else {
            let getter = getter.expect("validated compound property assignment has a getter");
            let current_local = self.new_temporary(type_.clone());
            let current_place = mir::Place::Local(current_local);
            self.instruction(mir::Instruction::Call {
                destination: Some(current_place.clone()),
                function: getter,
                arguments: vec![receiver.clone()],
                return_type: type_.clone(),
            });
            let right = self
                .lower_expression(value)
                .expect("property assignment value produces a value");
            let left = mir::Operand {
                type_: type_.clone(),
                kind: mir::OperandKind::Copy(current_place),
            };
            if type_ == &hir::Type::String && operator == hir::AssignmentOperator::AddAssign {
                self.emit_string_concat(left, right)
            } else {
                self.temporary(
                    type_.clone(),
                    mir::RvalueKind::Binary {
                        left,
                        operator: compound_operator(operator),
                        right,
                    },
                )
            }
        };
        self.instruction(mir::Instruction::Call {
            destination: None,
            function: setter,
            arguments: vec![receiver, assigned.clone()],
            return_type: mir::Type::Void,
        });
        assigned
    }

    fn place(&mut self, expression: &hir::Expression) -> mir::Place {
        match &expression.kind {
            hir::ExpressionKind::Symbol(symbol) => self
                .symbol_locals
                .get(symbol)
                .copied()
                .map_or(mir::Place::Symbol(*symbol), mir::Place::Local),
            hir::ExpressionKind::Member { object, symbol } => {
                if matches!(object.type_, hir::Type::Class(_)) {
                    mir::Place::ObjectField {
                        object: Box::new(
                            self.lower_expression(object)
                                .expect("object receiver produces a value"),
                        ),
                        field: *symbol,
                    }
                } else {
                    mir::Place::Field {
                        base: Box::new(self.place(object)),
                        field: *symbol,
                    }
                }
            }
            hir::ExpressionKind::Index { array, index } => mir::Place::Index {
                array: Box::new(
                    self.lower_expression(array)
                        .expect("validated array produces a value"),
                ),
                index: Box::new(
                    self.lower_expression(index)
                        .expect("validated index produces a value"),
                ),
                element_type: expression.type_.clone(),
            },
            _ => panic!("validated assignment has a place expression"),
        }
    }

    pub(super) fn symbol_operand(&self, symbol: hir::SymbolId, type_: &hir::Type) -> mir::Operand {
        let place = self
            .symbol_locals
            .get(&symbol)
            .copied()
            .map_or(mir::Place::Symbol(symbol), mir::Place::Local);
        mir::Operand {
            type_: type_.clone(),
            kind: mir::OperandKind::Copy(place),
        }
    }
}
