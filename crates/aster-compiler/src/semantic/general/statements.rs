use super::{
    Analyzer, Binding, Block, ConstError, ConstValue, Diagnostic, EnumCaseInfo, Expression,
    HashMap, HashSet, ResolvedEnumCase, Span, Statement, Type, TypeRef, VariableDeclaration,
    VariableKind, evaluate, resolve_type_readonly,
};

pub(super) struct ResolvedSwitch<'a> {
    pub(super) enum_name: &'a str,
    pub(super) cases: &'a [EnumCaseInfo],
    pub(super) indices: &'a HashMap<&'a str, usize>,
}

/// Compile-time identity of an integer, `char` or `string` switch label;
/// `char` labels compare by scalar value like integers.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum ScalarSwitchKey {
    Integer(i128),
    String(String),
}

/// Integer, `char` and `string` values may be switched on with constant
/// labels (and a mandatory `default`), in addition to enums.
pub(super) fn is_scalar_switch_subject(type_: &Type) -> bool {
    matches!(type_, Type::Char | Type::String)
        || type_
            .primitive()
            .is_some_and(aster_types::Primitive::is_integer)
}

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
                        let actual = self.expression_expected(value, Some(&expected));
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
                                    "constructor returns before {} is initialized",
                                    super::field_display(field)
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
            Statement::Unsafe { body, .. } => {
                self.unsafe_depth += 1;
                let flow = self.block(body, true);
                self.unsafe_depth -= 1;
                flow
            }
            Statement::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                // Pattern variables declared by the condition belong to the
                // enclosing scope (as in C#), but are definitely assigned only
                // where the condition proves the test succeeded.
                let condition_type = self.expression(condition);
                self.require_bool_condition("if", &condition_type, condition.span);
                let when_true = pattern_assigned(condition, true);
                let when_false = pattern_assigned(condition, false);
                let before = self.scopes.clone();
                self.mark_assigned(&when_true);
                let then_flow = self.block(then_block, true);
                let then_scopes = self.scopes.clone();
                self.scopes.clone_from(&before);
                self.mark_assigned(&when_false);
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
                // Pattern variables of a loop condition are scoped to the loop.
                self.scopes.push(HashMap::new());
                let condition_type = self.expression(condition);
                self.require_bool_condition("while", &condition_type, condition.span);
                self.loop_depth += 1;
                let before = self.scopes.clone();
                self.mark_assigned(&pattern_assigned(condition, true));
                self.block(body, true);
                self.scopes = before;
                self.loop_depth -= 1;
                self.scopes.pop();
                Flow::CONTINUE
            }
            Statement::DoWhile {
                body, condition, ..
            } => {
                self.loop_depth += 1;
                let before = self.scopes.clone();
                self.block(body, true);
                self.scopes = before;
                self.loop_depth -= 1;
                self.scopes.push(HashMap::new());
                let condition_type = self.expression(condition);
                self.require_bool_condition("do-while", &condition_type, condition.span);
                self.scopes.pop();
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
                let mut assigned = Vec::new();
                if let Some(condition) = condition {
                    let condition_type = self.expression(condition);
                    self.require_bool_condition("for", &condition_type, condition.span);
                    assigned = pattern_assigned(condition, true);
                }
                let before_update = self.scopes.clone();
                self.mark_assigned(&assigned);
                if let Some(update) = update {
                    self.expression(update);
                }
                self.loop_depth += 1;
                let before_body = self.scopes.clone();
                self.block(body, true);
                self.scopes = before_body;
                self.scopes = before_update;
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
                let collection_is_string = collection_type == Type::String;
                let actual_type = match collection_type {
                    Type::Array(element) | Type::List(element) => *element,
                    // Iterating a `string` produces Unicode scalar values,
                    // not bytes/UTF-16 units/grapheme clusters, so the
                    // element type is always exactly `char` -- never a type
                    // parsed out of the collection expression itself, since
                    // `string` has no declared element type to extract.
                    Type::String => Type::Char,
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
                let declared_type = element_type.as_ref().map_or_else(
                    || actual_type.clone(),
                    |element_type| self.resolve_local_type(element_type),
                );
                if actual_type != Type::Unknown
                    && declared_type != Type::Unknown
                    && actual_type != declared_type
                {
                    if collection_is_string {
                        self.diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "foreach over string requires element type `char`, found `{}`",
                                    declared_type.display()
                                ),
                                element_type
                                    .as_ref()
                                    .map_or(collection.span, |value| value.span),
                            )
                            .with_help("declare the foreach variable as `char`"),
                        );
                    } else {
                        self.diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "foreach element type `{}` does not match array element type `{}`",
                                    declared_type.display(),
                                    actual_type.display()
                                ),
                                element_type.as_ref().map_or(collection.span, |value| value.span),
                            )
                            .with_help("use the array element type for the foreach variable"),
                        );
                    }
                }
                self.scopes.push(HashMap::new());
                self.declare(
                    element_name,
                    Binding {
                        type_: declared_type,
                        mutable: false,
                        iteration_readonly: true,
                        initialized: true,
                        span: element_type
                            .as_ref()
                            .map_or(collection.span, |value| value.span),
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
        if is_scalar_switch_subject(&selected) {
            let mut seen = HashSet::new();
            let mut any_continues = false;
            for case in cases {
                // A guarded case can fall through, so its label is only checked
                // against earlier unguarded cases and is not recorded.
                if case.guard.is_some() {
                    let mut scratch = seen.clone();
                    self.check_scalar_label(
                        &selected,
                        case.label.as_ref(),
                        case.span,
                        &mut scratch,
                    );
                } else {
                    self.check_scalar_label(&selected, case.label.as_ref(), case.span, &mut seen);
                }
                self.scopes.push(HashMap::new());
                let assigned = self.analyze_guard(case.guard.as_ref());
                self.mark_assigned(&assigned);
                any_continues |= self.block(&case.body, true).can_continue;
                self.scopes.pop();
            }
            if let Some(default) = default {
                any_continues |= self.block(default, true).can_continue;
            } else {
                self.require_scalar_default(&selected, span);
                any_continues = true;
            }
            return Flow {
                can_continue: any_continues,
            };
        }
        let Type::Enum(enum_name) = selected else {
            if selected != Type::Unknown {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`switch` requires an enum, integer, `char` or `string` value, found `{}`",
                            selected.display()
                        ),
                        value.span,
                    )
                    .with_help("select an enum, integer, `char` or `string` value"),
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
        let case_indices = enum_cases
            .iter()
            .enumerate()
            .map(|(index, case)| (case.name.as_str(), index))
            .collect::<HashMap<_, _>>();
        let resolved = ResolvedSwitch {
            enum_name: &enum_name,
            cases: &enum_cases,
            indices: &case_indices,
        };
        let mut covered = HashSet::new();
        let mut any_continues = false;
        for case in cases {
            // A guarded case can fall through, so it never counts as covering
            // its enum case; it is still checked against the unguarded cases.
            let mut scratch = covered.clone();
            let Some(info) = self.resolve_switch_pattern(
                case.enum_name.as_deref(),
                &case.case_name,
                &case.bindings,
                case.span,
                &resolved,
                if case.guard.is_some() {
                    &mut scratch
                } else {
                    &mut covered
                },
            ) else {
                self.block(&case.body, true);
                any_continues = true;
                continue;
            };
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
            let assigned = self.analyze_guard(case.guard.as_ref());
            self.mark_assigned(&assigned);
            let flow = self.block(&case.body, false);
            self.scopes.pop();
            any_continues |= flow.can_continue;
        }
        if let Some(default) = default {
            self.validate_switch_coverage(&enum_cases, &covered, Some(default.span), span);
            any_continues |= self.block(default, true).can_continue;
        } else if !self.validate_switch_coverage(&enum_cases, &covered, None, span) {
            any_continues = true;
        }
        Flow {
            can_continue: any_continues,
        }
    }

    /// Validate one constant label of an integer, `char` or `string` switch:
    /// it must convert implicitly to the subject type (integer constants that
    /// fit the subject range convert), be a compile-time constant, and not
    /// repeat an earlier label.
    pub(super) fn check_scalar_label(
        &mut self,
        subject: &Type,
        label: Option<&Expression>,
        span: Span,
        seen: &mut HashSet<ScalarSwitchKey>,
    ) {
        let Some(label) = label else {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "a `{}` switch case must be a constant label, not an enum pattern",
                        subject.display()
                    ),
                    span,
                )
                .with_help("use a literal or constant of the switch value's type"),
            );
            return;
        };
        let label_type = self.expression(label);
        if label_type == Type::Unknown {
            return;
        }
        let errors_before = self.diagnostics.len();
        self.require_assignable_value(subject, &label_type, label);
        if self.diagnostics.len() != errors_before {
            return;
        }
        let resolve = |name: &str| self.binding(name).and_then(|binding| binding.value.clone());
        let value = match evaluate(label, &resolve) {
            Ok(value) => value,
            Err(error) => {
                let diagnostic = match error {
                    ConstError::NotConstant(span) => {
                        Diagnostic::error("a switch case label must be a constant expression", span)
                            .with_help(
                                "use literals, constants, operators, `?:`, or casts; calls and variables are not constant",
                            )
                    }
                    ConstError::Overflow(span, type_name) => Diagnostic::error(
                        format!("constant expression overflows `{type_name}`"),
                        span,
                    ),
                    ConstError::DivisionByZero(span) => {
                        Diagnostic::error("constant expression divides by zero", span)
                    }
                };
                self.diagnostics.push(diagnostic);
                return;
            }
        };
        let (key, shown) = match value {
            ConstValue::Integer(value, _) => (ScalarSwitchKey::Integer(value), value.to_string()),
            ConstValue::Char(value) => (
                ScalarSwitchKey::Integer(i128::from(u32::from(value))),
                format!("'{}'", value.escape_default()),
            ),
            ConstValue::Str(value) => (
                ScalarSwitchKey::String(value.clone()),
                format!("\"{}\"", value.escape_default()),
            ),
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "a switch case label cannot have type `{}` here",
                        label_type.display()
                    ),
                    label.span,
                ));
                return;
            }
        };
        if !seen.insert(key) {
            self.diagnostics.push(Diagnostic::error(
                format!("duplicate switch case `{shown}`"),
                span,
            ));
        }
    }

    /// Only enums can be exhaustive, so an integer, `char` or `string` switch
    /// always needs `default`.
    pub(super) fn require_scalar_default(&mut self, subject: &Type, span: Span) {
        self.diagnostics.push(
            Diagnostic::error(
                format!(
                    "a `switch` on `{}` requires a `default` case",
                    subject.display()
                ),
                span,
            )
            .with_help("only enum switches can be exhaustive; add a `default` case"),
        );
    }

    pub(super) fn resolve_switch_pattern(
        &mut self,
        owner: Option<&str>,
        case_name: &str,
        bindings: &[String],
        span: Span,
        resolved: &ResolvedSwitch<'_>,
        covered: &mut HashSet<usize>,
    ) -> Option<EnumCaseInfo> {
        if let Some(owner) = owner
            && owner != resolved.enum_name
        {
            self.diagnostics.push(Diagnostic::error(
                format!(
                    "case `{case_name}` belongs to `{owner}`, not `{}`",
                    resolved.enum_name
                ),
                span,
            ));
        }
        let Some(&case_index) = resolved.indices.get(case_name) else {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("enum `{}` has no case `{case_name}`", resolved.enum_name),
                    span,
                )
                .with_help("use one of the cases declared by the selected enum"),
            );
            return None;
        };
        let info = &resolved.cases[case_index];
        if !covered.insert(case_index) {
            self.diagnostics.push(Diagnostic::error(
                format!("duplicate switch case `{case_name}`"),
                span,
            ));
        }
        if bindings.len() != info.fields.len() {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "case `{case_name}` expects {} binding(s), found {}",
                        info.fields.len(),
                        bindings.len()
                    ),
                    span,
                )
                .with_help("bind each payload value exactly once"),
            );
        }
        self.model.switch_cases.insert(
            self.model_key(span),
            ResolvedEnumCase {
                enum_name: resolved.enum_name.to_owned(),
                case_index,
                argument_order: Vec::new(),
            },
        );
        Some(info.clone())
    }

    pub(super) fn validate_switch_coverage(
        &mut self,
        enum_cases: &[EnumCaseInfo],
        covered: &HashSet<usize>,
        default_span: Option<Span>,
        span: Span,
    ) -> bool {
        if let Some(default_span) = default_span {
            if covered.len() == enum_cases.len() {
                self.diagnostics.push(
                    Diagnostic::warning("unreachable `default` case", default_span)
                        .with_help("remove `default`; every enum case is already covered"),
                );
            }
            return true;
        }
        if covered.len() == enum_cases.len() {
            return true;
        }
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
        false
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
        let declared_type = match &variable.kind {
            VariableKind::Explicit(type_ref) | VariableKind::Constant(type_ref) => {
                Some(self.resolve_local_type(type_ref))
            }
            VariableKind::Inferred => None,
        };
        let initializer_type = variable
            .initializer
            .as_ref()
            .map(|value| self.expression_expected(value, declared_type.as_ref()));
        let (type_, mutable) = match &variable.kind {
            VariableKind::Explicit(_) => {
                let expected = declared_type.expect("explicit variable has a declared type");
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
                let expected = declared_type.expect("constant has a declared type");
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

    /// A `when` guard: a `bool` expression evaluated after the pattern
    /// matched. Returns the pattern variables assigned when it is true.
    pub(super) fn analyze_guard(&mut self, guard: Option<&Expression>) -> Vec<String> {
        let Some(guard) = guard else {
            return Vec::new();
        };
        let guard_type = self.expression(guard);
        self.require_bool_condition("when", &guard_type, guard.span);
        pattern_assigned(guard, true)
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

/// Names of the pattern variables that are definitely assigned when
/// `expression` evaluates to `when_true` (C# definite assignment for `is`
/// patterns through `!`, `&&` and `||`).
pub(super) fn pattern_assigned(expression: &Expression, when_true: bool) -> Vec<String> {
    use aster_syntax::{BinaryOperator, ExpressionKind, UnaryOperator};
    match &expression.kind {
        ExpressionKind::TypePattern { binding, .. } => {
            if when_true {
                binding.iter().map(|binding| binding.name.clone()).collect()
            } else {
                Vec::new()
            }
        }
        ExpressionKind::Unary {
            operator: UnaryOperator::Not,
            operand,
        } => pattern_assigned(operand, !when_true),
        ExpressionKind::Binary {
            left,
            operator: operator @ (BinaryOperator::LogicalAnd | BinaryOperator::LogicalOr),
            right,
        } => {
            let left = pattern_assigned(left, when_true);
            let right = pattern_assigned(right, when_true);
            // `a && b` is true only if both were, and `a || b` is false only
            // if both were: the union. Otherwise either operand may have
            // decided the result: the intersection.
            if (*operator == BinaryOperator::LogicalAnd) == when_true {
                let mut union = left;
                union.extend(right);
                union
            } else {
                left.into_iter()
                    .filter(|name| right.contains(name))
                    .collect()
            }
        }
        _ => Vec::new(),
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
