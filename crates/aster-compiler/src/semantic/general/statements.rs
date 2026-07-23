use super::{
    Analyzer, Binding, Block, ConstError, ConstValue, Diagnostic, Expression, HashMap, HashSet,
    ResolvedEnumCase, Span, Statement, Type, TypeRef, VariableDeclaration, VariableKind, evaluate,
    resolve_type_readonly,
};

#[derive(Clone, Copy)]
pub(super) struct Flow {
    pub(super) can_continue: bool,
}

impl Flow {
    pub(super) const CONTINUE: Self = Self { can_continue: true };
    pub(super) const TERMINATE: Self = Self {
        can_continue: false,
    };
}

impl Analyzer<'_> {
    pub(super) fn block(&mut self, block: &Block, create_scope: bool) -> Flow {
        if create_scope {
            self.scopes.push(HashMap::new());
        }
        let mut flow = Flow::CONTINUE;
        for statement in &block.statements {
            if !flow.can_continue {
                self.diagnostics.push(
                    Diagnostic::warning("unreachable code", statement.span())
                        .with_help("remove the statement or change the preceding control flow"),
                );
            }
            let statement_flow = self.statement(statement);
            if flow.can_continue {
                flow = statement_flow;
            }
        }
        if create_scope {
            self.scopes.pop();
        }
        flow
    }

    #[allow(clippy::too_many_lines)]
    fn statement(&mut self, statement: &Statement) -> Flow {
        match statement {
            Statement::Variable(variable) => {
                if let Some(binding) = self.variable_binding(variable) {
                    // Conservative v1 rule: a reference-typed local (of any
                    // inferred or explicit type) declared before the single
                    // `await` cannot cross the suspension.
                    if self.async_state == super::AsyncAnalysisState::BeforeAwait {
                        if super::declarations::is_reference_type(&binding.type_) {
                            self.diagnostics.push(
                                Diagnostic::error(
                                    "a reference-typed local cannot be declared before `await` in this version",
                                    variable.span,
                                )
                                .with_help("only scalar locals may cross an `await`"),
                            );
                        } else if binding.type_ != Type::Unknown
                            && !super::calls::transferable(&binding.type_)
                        {
                            // Not a reference type (caught above), but still not
                            // worker-transferable: `decimal` (no backend ABI yet)
                            // or an enum/struct value type.
                            self.diagnostics.push(
                                Diagnostic::error(
                                    format!(
                                        "a `{}` local cannot be declared before `await` in this version",
                                        binding.type_.display()
                                    ),
                                    variable.span,
                                )
                                .with_help("only scalar locals may cross an `await`"),
                            );
                        }
                    }
                    self.declare(&variable.name, binding);
                }
                Flow::CONTINUE
            }
            Statement::Return { value, span } => {
                match (&self.return_type, value) {
                    (Type::Void, Some(_)) => self.diagnostics.push(
                        Diagnostic::error("a `void` function cannot return a value", *span)
                            .with_help("use `return;` without a value"),
                    ),
                    (Type::Void, None) => {}
                    (_, None) => self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "return value of type `{}` is required",
                                self.return_type.display()
                            ),
                            *span,
                        )
                        .with_help("return an expression compatible with the function result type"),
                    ),
                    (expected, Some(value)) => {
                        let expected = expected.clone();
                        let actual = self.expression(value);
                        self.require_assignable_value(&expected, &actual, value);
                    }
                }
                if self.constructor {
                    for field in &self.field_names {
                        if self
                            .binding(field)
                            .is_some_and(|binding| !binding.initialized)
                        {
                            self.diagnostics.push(Diagnostic::error(
                                format!(
                                    "constructor returns before field `{field}` is initialized"
                                ),
                                *span,
                            ));
                        }
                    }
                }
                Flow::TERMINATE
            }
            Statement::Expression(expression) => {
                self.expression(expression);
                Flow::CONTINUE
            }
            Statement::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                let condition_type = self.expression(condition);
                self.require_bool_condition("if", &condition_type, condition.span);
                let before = self.scopes.clone();
                let then_flow = self.block(then_block, true);
                let then_scopes = self.scopes.clone();
                self.scopes.clone_from(&before);
                let else_flow = else_block
                    .as_ref()
                    .map_or(Flow::CONTINUE, |block| self.block(block, true));
                let else_scopes = self.scopes.clone();
                self.scopes = merge_branch_scopes(
                    &before,
                    &then_scopes,
                    then_flow.can_continue,
                    &else_scopes,
                    else_flow.can_continue,
                );
                Flow {
                    can_continue: then_flow.can_continue || else_flow.can_continue,
                }
            }
            Statement::While {
                condition, body, ..
            } => {
                let condition_type = self.expression(condition);
                self.require_bool_condition("while", &condition_type, condition.span);
                self.loop_depth += 1;
                let before = self.scopes.clone();
                self.block(body, true);
                self.scopes = before;
                self.loop_depth -= 1;
                Flow::CONTINUE
            }
            Statement::For {
                initializer,
                condition,
                update,
                body,
                ..
            } => {
                self.scopes.push(HashMap::new());
                if let Some(initializer) = initializer {
                    self.statement(initializer);
                }
                if let Some(condition) = condition {
                    let condition_type = self.expression(condition);
                    self.require_bool_condition("for", &condition_type, condition.span);
                }
                if let Some(update) = update {
                    self.expression(update);
                }
                self.loop_depth += 1;
                let before_body = self.scopes.clone();
                self.block(body, true);
                self.scopes = before_body;
                self.loop_depth -= 1;
                self.scopes.pop();
                Flow::CONTINUE
            }
            Statement::ForEach {
                element_type,
                element_name,
                collection,
                body,
                ..
            } => {
                let collection_type = self.expression(collection);
                let declared_type = self.resolve_local_type(element_type);
                let actual_type = match collection_type {
                    Type::Array(element) => *element,
                    Type::List(_) => {
                        self.diagnostics.push(
                            Diagnostic::error(
                                "foreach over `List<T>` is not supported yet",
                                collection.span,
                            )
                            .with_help("use an indexed loop until List<T> iteration is available"),
                        );
                        Type::Unknown
                    }
                    Type::String => {
                        self.diagnostics.push(
                            Diagnostic::error(
                                "foreach over `string` is not supported yet",
                                collection.span,
                            )
                            .with_help("use string operations until string iteration is available"),
                        );
                        Type::Unknown
                    }
                    Type::Unknown => Type::Unknown,
                    other => {
                        self.diagnostics.push(
                            Diagnostic::error(
                                format!("type `{}` is not iterable", other.display()),
                                collection.span,
                            )
                            .with_help("foreach currently accepts only arrays"),
                        );
                        Type::Unknown
                    }
                };
                if actual_type != Type::Unknown
                    && declared_type != Type::Unknown
                    && actual_type != declared_type
                {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "foreach element type `{}` does not match array element type `{}`",
                                declared_type.display(),
                                actual_type.display()
                            ),
                            element_type.span,
                        )
                        .with_help("use the array element type for the foreach variable"),
                    );
                }
                self.scopes.push(HashMap::new());
                self.declare(
                    element_name,
                    Binding {
                        type_: declared_type,
                        mutable: false,
                        iteration_readonly: true,
                        initialized: true,
                        span: element_type.span,
                        value: None,
                    },
                );
                self.loop_depth += 1;
                let before_body = self.scopes.clone();
                self.block(body, true);
                self.scopes = before_body;
                self.loop_depth -= 1;
                self.scopes.pop();
                Flow::CONTINUE
            }
            Statement::Switch {
                value,
                cases,
                default,
                span,
            } => self.switch_statement(value, cases, default.as_ref(), *span),
            Statement::Break(span) | Statement::Continue(span) => {
                if self.loop_depth == 0 {
                    let keyword = if matches!(statement, Statement::Break(_)) {
                        "break"
                    } else {
                        "continue"
                    };
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("`{keyword}` is only valid inside a loop"),
                            *span,
                        )
                        .with_help(format!("move `{keyword}` into a `while` or `for` loop")),
                    );
                    Flow::CONTINUE
                } else {
                    Flow::TERMINATE
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn switch_statement(
        &mut self,
        value: &Expression,
        cases: &[aster_syntax::SwitchCase],
        default: Option<&Block>,
        span: Span,
    ) -> Flow {
        let selected = self.expression(value);
        let Type::Enum(enum_name) = selected else {
            if selected != Type::Unknown {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`switch` requires an enum value, found `{}`",
                            selected.display()
                        ),
                        value.span,
                    )
                    .with_help("select a declared enum value"),
                );
            }
            for case in cases {
                self.block(&case.body, true);
            }
            if let Some(default) = default {
                self.block(default, true);
            }
            return Flow::CONTINUE;
        };
        let enum_cases = self
            .context
            .types
            .get(&enum_name)
            .map(|info| info.enum_cases.clone())
            .unwrap_or_default();
        let mut covered = HashSet::new();
        let mut any_continues = false;
        for case in cases {
            if let Some(owner) = &case.enum_name
                && owner != &enum_name
            {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "case `{}` belongs to `{owner}`, not `{enum_name}`",
                        case.case_name
                    ),
                    case.span,
                ));
            }
            let Some((case_index, info)) = enum_cases
                .iter()
                .enumerate()
                .find(|(_, item)| item.name == case.case_name)
            else {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("enum `{enum_name}` has no case `{}`", case.case_name),
                        case.span,
                    )
                    .with_help("use one of the cases declared by the selected enum"),
                );
                self.block(&case.body, true);
                any_continues = true;
                continue;
            };
            if !covered.insert(case_index) {
                self.diagnostics.push(Diagnostic::error(
                    format!("duplicate switch case `{}`", case.case_name),
                    case.span,
                ));
            }
            if case.bindings.len() != info.fields.len() {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "case `{}` expects {} binding(s), found {}",
                            case.case_name,
                            info.fields.len(),
                            case.bindings.len()
                        ),
                        case.span,
                    )
                    .with_help("bind each payload value exactly once"),
                );
            }
            self.model.switch_cases.insert(
                self.model_key(case.span),
                ResolvedEnumCase {
                    enum_name: enum_name.clone(),
                    case_index,
                },
            );
            self.scopes.push(HashMap::new());
            for (binding, (_, type_)) in case.bindings.iter().zip(&info.fields) {
                self.declare(
                    binding,
                    Binding {
                        type_: type_.clone(),
                        mutable: true,
                        iteration_readonly: false,
                        initialized: true,
                        span: case.span,
                        value: None,
                    },
                );
            }
            let flow = self.block(&case.body, false);
            self.scopes.pop();
            any_continues |= flow.can_continue;
        }
        if let Some(default) = default {
            if covered.len() == enum_cases.len() {
                self.diagnostics.push(
                    Diagnostic::warning("unreachable `default` case", default.span)
                        .with_help("remove `default`; every enum case is already covered"),
                );
            }
            any_continues |= self.block(default, true).can_continue;
        } else if covered.len() != enum_cases.len() {
            let missing = enum_cases
                .iter()
                .enumerate()
                .filter(|(index, _)| !covered.contains(index))
                .map(|(_, case)| case.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            self.diagnostics.push(
                Diagnostic::error(
                    format!("non-exhaustive switch; missing case(s): {missing}"),
                    span,
                )
                .with_help("handle every case or add a `default` arm"),
            );
            any_continues = true;
        }
        Flow {
            can_continue: any_continues,
        }
    }

    fn require_bool_condition(&mut self, construct: &str, actual: &Type, span: Span) {
        if *actual != Type::Bool && *actual != Type::Unknown {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`{construct}` condition must be `bool`, found `{}`",
                        actual.display()
                    ),
                    span,
                )
                .with_help("use a boolean expression as the condition"),
            );
        }
    }

    pub(super) fn variable_binding(&mut self, variable: &VariableDeclaration) -> Option<Binding> {
        let initializer_type = variable
            .initializer
            .as_ref()
            .map(|value| self.expression(value));
        let (type_, mutable) = match &variable.kind {
            VariableKind::Explicit(type_ref) => {
                let expected = self.resolve_local_type(type_ref);
                if let (Some(actual), Some(value)) = (&initializer_type, &variable.initializer) {
                    self.require_assignable_value(&expected, actual, value);
                }
                (expected, true)
            }
            VariableKind::Inferred => {
                let Some(type_) = initializer_type else {
                    self.diagnostics.push(
                        Diagnostic::error("`var` requires an initializer", variable.span)
                            .with_help("add `= expression` so the type can be inferred"),
                    );
                    return None;
                };
                (type_, true)
            }
            VariableKind::Constant(type_ref) => {
                let expected = self.resolve_local_type(type_ref);
                let Some(actual) = &initializer_type else {
                    self.diagnostics.push(
                        Diagnostic::error("constants require an initializer", variable.span)
                            .with_help("add a compile-time-compatible initializer"),
                    );
                    return None;
                };
                self.require_assignable_value(
                    &expected,
                    actual,
                    variable.initializer.as_ref().expect("checked above"),
                );
                let value = self.evaluate_constant(
                    variable.initializer.as_ref().expect("checked above"),
                    &type_ref.name,
                );
                return Some(Binding {
                    type_: expected,
                    mutable: false,
                    iteration_readonly: false,
                    initialized: true,
                    span: variable.span,
                    value,
                });
            }
        };
        Some(Binding {
            type_,
            mutable,
            iteration_readonly: false,
            initialized: variable.initializer.is_some(),
            span: variable.span,
            value: None,
        })
    }

    /// Evaluate a `const` initializer, reporting non-constant expressions,
    /// overflow, and division by zero.
    fn evaluate_constant(
        &mut self,
        initializer: &Expression,
        declared_type: &str,
    ) -> Option<ConstValue> {
        let resolve = |name: &str| self.binding(name).and_then(|binding| binding.value.clone());
        match evaluate(initializer, &resolve) {
            Ok(value) => Some(value.coerce_to(declared_type)),
            Err(ConstError::NotConstant(span)) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        "constant initializers must be compile-time constant expressions",
                        span,
                    )
                    .with_help(
                        "use literals, other constants, operators, `?:`, or casts; calls and variables are not constant",
                    ),
                );
                None
            }
            Err(ConstError::Overflow(span, type_name)) => {
                self.diagnostics.push(
                    Diagnostic::error(format!("constant expression overflows `{type_name}`"), span)
                        .with_help("adjust the expression so the value fits its type"),
                );
                None
            }
            Err(ConstError::DivisionByZero(span)) => {
                self.diagnostics.push(
                    Diagnostic::error("constant expression divides by zero", span)
                        .with_help("division and remainder by zero are undefined"),
                );
                None
            }
        }
    }

    pub(super) fn declare(&mut self, name: &str, binding: Binding) {
        let scope = self
            .scopes
            .last_mut()
            .expect("an analyzer always has a scope");
        if scope.contains_key(name) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("duplicate name `{name}` in this scope"),
                    binding.span,
                )
                .with_help("rename or remove one of the declarations"),
            );
        } else {
            scope.insert(name.to_owned(), binding);
        }
    }

    pub(super) fn resolve_local_type(&mut self, type_ref: &TypeRef) -> Type {
        let type_ = resolve_type_readonly(type_ref, self.context);
        if type_ == Type::Unknown {
            self.diagnostics.push(
                Diagnostic::error(format!("unknown type `{}`", type_ref.name), type_ref.span)
                    .with_help("declare the type or use a known basic type"),
            );
        } else if type_ == Type::Void {
            self.diagnostics.push(
                Diagnostic::error("variables cannot have type `void`", type_ref.span)
                    .with_help("use a value type for the variable"),
            );
        }
        type_
    }
}

fn merge_branch_scopes(
    before: &[HashMap<String, Binding>],
    then_scopes: &[HashMap<String, Binding>],
    then_continues: bool,
    else_scopes: &[HashMap<String, Binding>],
    else_continues: bool,
) -> Vec<HashMap<String, Binding>> {
    let mut merged = before.to_vec();
    for (index, scope) in merged.iter_mut().enumerate() {
        for (name, binding) in scope {
            let then_initialized = then_scopes
                .get(index)
                .and_then(|scope| scope.get(name))
                .is_some_and(|value| value.initialized);
            let else_initialized = else_scopes
                .get(index)
                .and_then(|scope| scope.get(name))
                .is_some_and(|value| value.initialized);
            binding.initialized = match (then_continues, else_continues) {
                (true, true) => then_initialized && else_initialized,
                (true, false) => then_initialized,
                (false, true) => else_initialized,
                (false, false) => binding.initialized,
            };
        }
    }
    merged
}
