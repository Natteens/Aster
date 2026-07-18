use super::declarations::visibility;
use super::types::{constant_expression, convert};
use super::{HashMap, Lowerer, ast, hir};
use crate::constexpr::evaluate;

impl Lowerer<'_> {
    pub(super) fn block(&mut self, block: &ast::Block) -> hir::Block {
        self.scopes.push(HashMap::new());
        let statements = block
            .statements
            .iter()
            .filter_map(|statement| self.statement(statement))
            .collect();
        self.scopes.pop();
        hir::Block { statements }
    }

    #[allow(clippy::too_many_lines)]
    fn statement(&mut self, statement: &ast::Statement) -> Option<hir::Statement> {
        Some(match statement {
            ast::Statement::Variable(variable) => {
                let symbol = self.allocate();
                let variable = self.variable(variable, symbol);
                self.scopes
                    .last_mut()?
                    .insert(variable.name.clone(), symbol);
                hir::Statement::Variable(variable)
            }
            ast::Statement::Return { value, .. } => {
                let target = self.current_return.clone();
                hir::Statement::Return(
                    value
                        .as_ref()
                        .map(|value| convert(self.expression(value), &target)),
                )
            }
            ast::Statement::Expression(expression) => {
                hir::Statement::Expression(self.expression(expression))
            }
            ast::Statement::If {
                condition,
                then_block,
                else_block,
                ..
            } => hir::Statement::If {
                condition: self.expression(condition),
                then_block: self.block(then_block),
                else_block: else_block.as_ref().map(|block| self.block(block)),
            },
            ast::Statement::While {
                condition, body, ..
            } => hir::Statement::While {
                condition: self.expression(condition),
                body: self.block(body),
            },
            ast::Statement::For {
                initializer,
                condition,
                update,
                body,
                ..
            } => {
                self.scopes.push(HashMap::new());
                let initializer = initializer
                    .as_ref()
                    .and_then(|value| self.statement(value))
                    .map(Box::new);
                let condition = condition.as_ref().map(|value| self.expression(value));
                let update = update.as_ref().map(|value| self.expression(value));
                let body = self.block(body);
                self.scopes.pop();
                hir::Statement::For {
                    initializer,
                    condition,
                    update,
                    body,
                }
            }
            ast::Statement::Switch {
                value,
                cases,
                default,
                ..
            } => {
                let value = self.expression(value);
                let mut lowered_cases = Vec::new();
                for case in cases {
                    let key = crate::semantic::ModelNodeKey {
                        context: self.model_context.clone(),
                        span: case.span,
                    };
                    let resolved = &self.model.switch_cases[&key];
                    let (case_symbol, field_symbols) =
                        self.enum_cases[&(resolved.enum_name.clone(), resolved.case_index)].clone();
                    self.scopes.push(HashMap::new());
                    let bindings = case
                        .bindings
                        .iter()
                        .zip(field_symbols)
                        .map(|(name, field)| {
                            let symbol = self.allocate();
                            let type_ = self.symbol_types[&field].clone();
                            self.symbol_types.insert(symbol, type_.clone());
                            self.scopes
                                .last_mut()
                                .expect("switch case scope")
                                .insert(name.clone(), symbol);
                            hir::Parameter {
                                symbol,
                                name: name.clone(),
                                type_,
                            }
                        })
                        .collect();
                    let body = self.block(&case.body);
                    self.scopes.pop();
                    lowered_cases.push(hir::SwitchCase {
                        case: case_symbol,
                        tag: u32::try_from(resolved.case_index).expect("validated enum tag"),
                        bindings,
                        body,
                    });
                }
                hir::Statement::Switch {
                    value,
                    cases: lowered_cases,
                    default: default.as_ref().map(|block| self.block(block)),
                }
            }
            ast::Statement::Break(_) => hir::Statement::Break,
            ast::Statement::Continue(_) => hir::Statement::Continue,
        })
    }

    pub(super) fn variable(
        &mut self,
        variable: &ast::VariableDeclaration,
        symbol: hir::SymbolId,
    ) -> hir::Variable {
        if let ast::VariableKind::Constant(type_ref) = &variable.kind
            && let Some(initializer) = &variable.initializer
        {
            let folded = {
                let resolve = |name: &str| {
                    self.lookup(name)
                        .and_then(|symbol| self.constant_values.get(&symbol).cloned())
                };
                evaluate(initializer, &resolve).ok()
            };
            if let Some(value) = folded {
                let value = value.coerce_to(&type_ref.name);
                let type_ = self.resolve_type(type_ref);
                self.constant_values.insert(symbol, value.clone());
                self.symbol_types.insert(symbol, type_.clone());
                return hir::Variable {
                    symbol,
                    name: variable.name.clone(),
                    visibility: variable.visibility.map(visibility),
                    type_,
                    mutable: false,
                    initializer: Some(constant_expression(&value)),
                };
            }
        }
        let mut initializer = variable
            .initializer
            .as_ref()
            .map(|value| self.expression(value));
        let type_ = match &variable.kind {
            ast::VariableKind::Inferred => initializer
                .as_ref()
                .map_or(hir::Type::Unknown, |value| value.type_.clone()),
            ast::VariableKind::Explicit(type_ref) | ast::VariableKind::Constant(type_ref) => {
                self.resolve_type(type_ref)
            }
        };
        initializer = initializer.map(|value| convert(value, &type_));
        self.symbol_types.insert(symbol, type_.clone());
        hir::Variable {
            symbol,
            name: variable.name.clone(),
            visibility: variable.visibility.map(visibility),
            type_,
            mutable: !matches!(variable.kind, ast::VariableKind::Constant(_)),
            initializer,
        }
    }
}
