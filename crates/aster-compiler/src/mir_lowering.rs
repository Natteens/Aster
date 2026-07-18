use std::collections::HashMap;

use aster_hir as hir;
use aster_mir as mir;

#[allow(clippy::too_many_lines)]
pub(crate) fn lower(module: &hir::Module) -> mir::Module {
    let intrinsics = module
        .items
        .iter()
        .flat_map(|item| match item {
            hir::Item::Function(function) => std::slice::from_ref(function),
            hir::Item::Class(declaration) | hir::Item::Struct(declaration) => {
                declaration.methods.as_slice()
            }
            hir::Item::Interface(_) | hir::Item::Enum(_) | hir::Item::Variable(_) => &[],
        })
        .filter_map(|function| {
            function
                .intrinsic
                .map(|intrinsic| (function.symbol, intrinsic))
        })
        .collect::<HashMap<_, _>>();
    let structs = module
        .items
        .iter()
        .filter_map(|item| {
            let hir::Item::Struct(declaration) = item else {
                return None;
            };
            Some(mir::StructDefinition {
                symbol: declaration.symbol,
                name: declaration.name.clone(),
                fields: declaration
                    .fields
                    .iter()
                    .map(|field| mir::FieldDefinition {
                        symbol: field.symbol,
                        name: field.name.clone(),
                        type_: field.type_.clone(),
                    })
                    .collect(),
            })
        })
        .collect();
    let classes = module
        .items
        .iter()
        .filter_map(|item| {
            let hir::Item::Class(declaration) = item else {
                return None;
            };
            Some(mir::ClassDefinition {
                symbol: declaration.symbol,
                name: declaration.name.clone(),
                fields: declaration
                    .fields
                    .iter()
                    .map(|field| mir::FieldDefinition {
                        symbol: field.symbol,
                        name: field.name.clone(),
                        type_: field.type_.clone(),
                    })
                    .collect(),
            })
        })
        .collect();
    let (interfaces, interface_implementations) = lower_interfaces(module);
    let enums = module
        .items
        .iter()
        .filter_map(|item| {
            let hir::Item::Enum(declaration) = item else {
                return None;
            };
            Some(mir::EnumDefinition {
                symbol: declaration.symbol,
                name: declaration.name.clone(),
                cases: declaration
                    .cases
                    .iter()
                    .map(|case| mir::EnumCaseDefinition {
                        symbol: case.symbol,
                        name: case.name.clone(),
                        tag: case.tag,
                        fields: case
                            .fields
                            .iter()
                            .map(|field| mir::FieldDefinition {
                                symbol: field.symbol,
                                name: field.name.clone(),
                                type_: field.type_.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    let enum_map = enums
        .iter()
        .map(|value| (value.symbol, value.clone()))
        .collect::<HashMap<_, _>>();
    let mut functions = Vec::new();
    for item in &module.items {
        match item {
            hir::Item::Function(function) => {
                push_function(&mut functions, function, None, &intrinsics, &enum_map);
            }
            hir::Item::Class(declaration) | hir::Item::Struct(declaration) => {
                for method in &declaration.methods {
                    push_function(
                        &mut functions,
                        method,
                        Some(declaration.symbol),
                        &intrinsics,
                        &enum_map,
                    );
                }
            }
            hir::Item::Interface(_) | hir::Item::Enum(_) | hir::Item::Variable(_) => {}
        }
    }
    mir::Module {
        structs,
        classes,
        interfaces,
        enums,
        interface_implementations,
        functions,
    }
}

fn lower_interfaces(
    module: &hir::Module,
) -> (
    Vec<mir::InterfaceDefinition>,
    Vec<mir::InterfaceImplementation>,
) {
    let interfaces = module
        .items
        .iter()
        .filter_map(|item| {
            let hir::Item::Interface(declaration) = item else {
                return None;
            };
            Some(mir::InterfaceDefinition {
                symbol: declaration.symbol,
                name: declaration.name.clone(),
                methods: declaration
                    .methods
                    .iter()
                    .map(|method| mir::InterfaceMethodDefinition {
                        symbol: method.symbol,
                        name: method.name.clone(),
                        parameters: method
                            .parameters
                            .iter()
                            .map(|parameter| parameter.type_.clone())
                            .collect(),
                        return_type: method.return_type.clone(),
                    })
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    let interface_methods = interfaces
        .iter()
        .map(|interface| (interface.symbol, interface.methods.clone()))
        .collect::<HashMap<_, _>>();
    let mut interface_implementations = Vec::new();
    for item in &module.items {
        let hir::Item::Class(declaration) = item else {
            continue;
        };
        for interface in &declaration.interfaces {
            let methods = interface_methods[interface]
                .iter()
                .map(|required| {
                    declaration
                        .methods
                        .iter()
                        .find(|method| {
                            method.name == required.name
                                && method.return_type == required.return_type
                                && method
                                    .parameters
                                    .iter()
                                    .skip(1)
                                    .map(|parameter| &parameter.type_)
                                    .eq(required.parameters.iter())
                        })
                        .expect("validated class implements every interface method")
                        .symbol
                })
                .collect();
            interface_implementations.push(mir::InterfaceImplementation {
                class: declaration.symbol,
                interface: *interface,
                methods,
            });
        }
    }
    (interfaces, interface_implementations)
}

fn push_function(
    functions: &mut Vec<mir::Function>,
    function: &hir::Function,
    owner: Option<hir::SymbolId>,
    intrinsics: &HashMap<hir::SymbolId, hir::Intrinsic>,
    enums: &HashMap<hir::SymbolId, mir::EnumDefinition>,
) {
    if function.body.is_some() && function.intrinsic.is_none() {
        functions.push(
            FunctionLowerer::new(function, intrinsics.clone(), enums.clone())
                .lower(function, owner),
        );
    }
}

struct PendingBlock {
    id: mir::BasicBlockId,
    instructions: Vec<mir::Instruction>,
    terminator: Option<mir::Terminator>,
}

#[derive(Clone, Copy)]
struct LoopTargets {
    break_block: mir::BasicBlockId,
    continue_block: mir::BasicBlockId,
}

struct FunctionLowerer {
    blocks: Vec<PendingBlock>,
    current: Option<mir::BasicBlockId>,
    parameters: Vec<mir::Local>,
    locals: Vec<mir::Local>,
    symbol_locals: HashMap<hir::SymbolId, mir::LocalId>,
    loops: Vec<LoopTargets>,
    next_local: u32,
    next_temporary: u32,
    intrinsics: HashMap<hir::SymbolId, hir::Intrinsic>,
    enums: HashMap<hir::SymbolId, mir::EnumDefinition>,
}

impl FunctionLowerer {
    fn new(
        function: &hir::Function,
        intrinsics: HashMap<hir::SymbolId, hir::Intrinsic>,
        enums: HashMap<hir::SymbolId, mir::EnumDefinition>,
    ) -> Self {
        let mut lowerer = Self {
            blocks: Vec::new(),
            current: None,
            parameters: Vec::new(),
            locals: Vec::new(),
            symbol_locals: HashMap::new(),
            loops: Vec::new(),
            next_local: 0,
            next_temporary: 0,
            intrinsics,
            enums,
        };
        let entry = lowerer.new_block();
        lowerer.current = Some(entry);
        for parameter in &function.parameters {
            let local = lowerer.source_local(
                parameter.symbol,
                parameter.name.clone(),
                parameter.type_.clone(),
                true,
            );
            lowerer.parameters.push(local);
        }
        lowerer
    }

    fn lower(mut self, function: &hir::Function, owner: Option<hir::SymbolId>) -> mir::Function {
        if let Some(body) = &function.body {
            self.lower_block(body);
        }
        if let Some(current) = self.current {
            self.terminate(current, mir::Terminator::End);
        }
        let blocks = self
            .blocks
            .into_iter()
            .map(|block| mir::BasicBlock {
                id: block.id,
                instructions: block.instructions,
                terminator: block.terminator.unwrap_or(mir::Terminator::End),
            })
            .collect();
        mir::Function {
            constructor: function.constructor,
            symbol: function.symbol,
            owner,
            name: function.name.clone(),
            visibility: function.visibility,
            parameters: self.parameters,
            locals: self.locals,
            return_type: function.return_type.clone(),
            entry: mir::BasicBlockId(0),
            blocks,
        }
    }

    fn lower_block(&mut self, block: &hir::Block) {
        for statement in &block.statements {
            if self.current.is_none() {
                break;
            }
            self.lower_statement(statement);
        }
    }

    fn lower_statement(&mut self, statement: &hir::Statement) {
        match statement {
            hir::Statement::Variable(variable) => self.lower_variable(variable),
            hir::Statement::Return(value) => {
                let value = value
                    .as_ref()
                    .and_then(|value| self.lower_expression(value));
                self.terminate_current(mir::Terminator::Return(value));
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
    fn lower_propagate_result(
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

    #[allow(clippy::too_many_lines)]
    fn lower_expression(&mut self, expression: &hir::Expression) -> Option<mir::Operand> {
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

    fn place_operand(&mut self, expression: &hir::Expression) -> mir::Operand {
        mir::Operand {
            type_: expression.type_.clone(),
            kind: mir::OperandKind::Copy(self.place(expression)),
        }
    }

    fn lower_call(
        &mut self,
        callee: &hir::Expression,
        arguments: &[hir::Expression],
        return_type: &hir::Type,
    ) -> Option<mir::Operand> {
        let function = expression_symbol(callee).expect("validated call has a resolved symbol");
        if let Some(intrinsic) = self.intrinsics.get(&function).copied() {
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
            return None;
        }
        if let hir::ExpressionKind::Member { object, .. } = &callee.kind
            && matches!(object.type_, hir::Type::Interface(_))
        {
            let receiver = self
                .lower_expression(object)
                .expect("interface method receiver produces a value");
            let lowered_arguments = arguments
                .iter()
                .filter_map(|argument| self.lower_expression(argument))
                .collect();
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
        lowered_arguments.extend(
            arguments
                .iter()
                .filter_map(|argument| self.lower_expression(argument)),
        );
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

    /// Prefix forms produce the updated value; postfix forms produce the value
    /// observed before the update.
    fn lower_increment_decrement(
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

    fn lower_assignment(
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

    fn lower_property_assignment(
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

    fn emit_string_concat(&mut self, left: mir::Operand, right: mir::Operand) -> mir::Operand {
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

    fn symbol_operand(&self, symbol: hir::SymbolId, type_: &hir::Type) -> mir::Operand {
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

    fn temporary(&mut self, type_: hir::Type, kind: mir::RvalueKind) -> mir::Operand {
        let local = self.new_temporary(type_.clone());
        self.assign(
            mir::Place::Local(local),
            mir::Rvalue {
                type_: type_.clone(),
                kind,
            },
        );
        mir::Operand {
            type_,
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
        }
    }

    fn new_temporary(&mut self, type_: hir::Type) -> mir::LocalId {
        let id = self.allocate_local();
        let name = format!("_tmp{}", self.next_temporary);
        self.next_temporary += 1;
        self.locals.push(mir::Local {
            id,
            symbol: None,
            name,
            type_,
            mutable: true,
            temporary: true,
        });
        id
    }

    fn source_local(
        &mut self,
        symbol: hir::SymbolId,
        name: String,
        type_: hir::Type,
        mutable: bool,
    ) -> mir::Local {
        let id = self.allocate_local();
        self.symbol_locals.insert(symbol, id);
        mir::Local {
            id,
            symbol: Some(symbol),
            name,
            type_,
            mutable,
            temporary: false,
        }
    }

    fn allocate_local(&mut self) -> mir::LocalId {
        let id = mir::LocalId(self.next_local);
        self.next_local += 1;
        id
    }

    fn new_block(&mut self) -> mir::BasicBlockId {
        let id =
            mir::BasicBlockId(u32::try_from(self.blocks.len()).expect("MIR block count fits u32"));
        self.blocks.push(PendingBlock {
            id,
            instructions: Vec::new(),
            terminator: None,
        });
        id
    }

    fn assign(&mut self, target: mir::Place, value: mir::Rvalue) {
        self.instruction(mir::Instruction::Assign { target, value });
    }

    fn instruction(&mut self, instruction: mir::Instruction) {
        let current = self.current.expect("instructions require a live block");
        self.blocks[current.0 as usize]
            .instructions
            .push(instruction);
    }

    fn terminate_current(&mut self, terminator: mir::Terminator) {
        let current = self
            .current
            .take()
            .expect("terminator requires a live block");
        self.terminate(current, terminator);
    }

    fn terminate(&mut self, block: mir::BasicBlockId, terminator: mir::Terminator) {
        let slot = &mut self.blocks[block.0 as usize].terminator;
        assert!(slot.is_none(), "a basic block has one terminator");
        *slot = Some(terminator);
    }
}

fn lower_intrinsic(intrinsic: hir::Intrinsic) -> mir::Intrinsic {
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

fn expression_symbol(expression: &hir::Expression) -> Option<hir::SymbolId> {
    match expression.kind {
        hir::ExpressionKind::Symbol(symbol) | hir::ExpressionKind::Member { symbol, .. } => {
            Some(symbol)
        }
        _ => None,
    }
}

fn one_constant(type_: &hir::Type) -> mir::Constant {
    match type_ {
        hir::Type::Float | hir::Type::Double => mir::Constant::Float("1".to_owned()),
        _ => mir::Constant::Integer("1".to_owned()),
    }
}

fn boolean_operand(value: bool) -> mir::Operand {
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

fn compound_operator(operator: hir::AssignmentOperator) -> mir::BinaryOperator {
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
