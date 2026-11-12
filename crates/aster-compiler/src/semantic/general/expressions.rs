use super::statements::ResolvedSwitch;
use super::{
    Analyzer, AssignmentOperator, BinaryOperator, Binding, Diagnostic, Expression, ExpressionKind,
    HashMap, HashSet, IncrementOperator, IntegerFit, InterpolatedPart, Literal, MemberDispatch,
    MemberInitializer, OptionCases, Primitive, PropertyInfo, ResolvedPropagation,
    ResolvedPropertyAssignment, ResolvedTypeAs, ResultCases, Span, Type, TypeKind, TypeName,
    TypeRef, UnaryOperator, UnsignedFit, Visibility, classify_integer, classify_unsigned,
    compatible, constant_integer, evaluate, fits_long, fits_ulong, integer_value, promoted_numeric,
    resolve_type_readonly, zero_initializable,
};
use crate::semantic::{INITIALIZER_RECEIVER, initializer_assignment};

impl Analyzer<'_> {
    /// Whether `name`'s enum shares the nominal identity of the discovered
    /// official `aster.core.Result`. `false` when the core stdlib is absent.
    fn is_official_result(&self, name: &str) -> bool {
        self.context.official_result.as_deref()
            == TypeName::parse(name).map(|parsed| parsed.base).as_deref()
            && self.context.official_result.is_some()
    }

    pub(super) fn is_official_option(&self, name: &str) -> bool {
        self.context.official_option.as_deref()
            == TypeName::parse(name).map(|parsed| parsed.base).as_deref()
            && self.context.official_option.is_some()
    }

    /// Analyze a postfix `?`: verify the operand is the official
    /// `aster.core.Result`, that the enclosing function returns a `Result` with
    /// an exactly matching error type, and record the concrete resolution for
    /// HIR lowering. Returns the success type `T`.
    fn await_expression(&mut self, operand: &Expression, span: Span) -> Type {
        let operand_type = self.expression(operand);
        if self.async_state == super::AsyncAnalysisState::OutsideAsync {
            self.diagnostics.push(
                Diagnostic::error("`await` is only valid inside an `async` function", span)
                    .with_help("mark the enclosing function `async Task<T>`"),
            );
            return Type::Unknown;
        }
        self.async_state = super::AsyncAnalysisState::AfterAwait;
        if let Type::Task(result_type) = operand_type {
            return *result_type;
        }
        if operand_type != Type::Unknown {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`await` requires a `Task<T>` operand, found `{}`",
                        operand_type.display()
                    ),
                    span,
                )
                .with_help("await the result of `Task.Run(...)`"),
            );
        }
        Type::Unknown
    }

    fn try_propagate(&mut self, operand: &Expression, span: Span) -> Type {
        let operand_type = self.expression(operand);
        let Type::Enum(operand_name) = operand_type else {
            if operand_type != Type::Unknown {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`?` requires an `aster.core.Result<T, E>` or `aster.core.Option<T>` value, found `{}`",
                            operand_type.display()
                        ),
                        span,
                    )
                    .with_help("`?` propagates the `Error` case of a `Result` or the `None` case of an `Option`"),
                );
            }
            return Type::Unknown;
        };
        if self.is_official_result(&operand_name) {
            return self.try_propagate_result(operand_name, span);
        }
        if self.is_official_option(&operand_name) {
            return self.try_propagate_option(operand_name, span);
        }
        self.diagnostics.push(
            Diagnostic::error(
                format!(
                    "`?` works only with the official `aster.core.Result<T, E>` or `aster.core.Option<T>`, not `{operand_name}`"
                ),
                span,
            )
            .with_help(
                "only the official `aster.core.Result`/`aster.core.Option` support `?`; a \
                 same-named or structurally similar enum does not qualify",
            ),
        );
        Type::Unknown
    }

    fn try_propagate_result(&mut self, result_name: String, span: Span) -> Type {
        let Some(operand_result) = self.result_cases(&result_name) else {
            self.internal_result_error(&result_name, span);
            return Type::Unknown;
        };
        let success_type = operand_result.success.clone();
        if self.model_context.starts_with("#global:") {
            self.diagnostics.push(
                Diagnostic::error("`?` cannot be used outside a function".to_owned(), span)
                    .with_help("`?` needs an enclosing function to receive the `Error` return"),
            );
            return success_type;
        }
        let Type::Enum(return_name) = self.return_type.clone() else {
            self.require_result_return(&operand_result.error, span);
            return success_type;
        };
        if !self.is_official_result(&return_name) {
            self.require_result_return(&operand_result.error, span);
            return success_type;
        }
        let Some(function_result) = self.result_cases(&return_name) else {
            self.internal_result_error(&return_name, span);
            return success_type;
        };
        if operand_result.error != function_result.error {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`?` cannot propagate error type `{}`; the enclosing function returns \
                         `Result<..., {}>`",
                        operand_result.error.display(),
                        function_result.error.display()
                    ),
                    span,
                )
                .with_help(
                    "convert the error explicitly with `switch`; `?` does not convert error types",
                ),
            );
            return success_type;
        }
        self.model.propagations.insert(
            self.model_key(span),
            ResolvedPropagation::Result {
                result_type: result_name,
                ok_index: operand_result.ok_index,
                error_index: operand_result.error_index,
                function_result_type: return_name,
                function_error_index: function_result.error_index,
            },
        );
        success_type
    }

    /// Analyze a postfix `?` whose operand is the official `aster.core.Option<T>`:
    /// verify the enclosing function returns the official `Option<U>` (`U`
    /// need not equal `T`, since `None` carries no payload to convert), and
    /// record the concrete resolution for HIR lowering. Returns `T`.
    fn try_propagate_option(&mut self, option_name: String, span: Span) -> Type {
        let Some(operand_option) = self.option_cases(&option_name) else {
            self.internal_option_error(&option_name, span);
            return Type::Unknown;
        };
        let success_type = operand_option.success.clone();
        if self.model_context.starts_with("#global:") {
            self.diagnostics.push(
                Diagnostic::error("`?` cannot be used outside a function".to_owned(), span)
                    .with_help("`?` needs an enclosing function to receive the `None` return"),
            );
            return success_type;
        }
        let Type::Enum(return_name) = self.return_type.clone() else {
            self.require_option_return(span);
            return success_type;
        };
        if !self.is_official_option(&return_name) {
            self.require_option_return(span);
            return success_type;
        }
        let Some(function_option) = self.option_cases(&return_name) else {
            self.internal_option_error(&return_name, span);
            return success_type;
        };
        self.model.propagations.insert(
            self.model_key(span),
            ResolvedPropagation::Option {
                option_type: option_name,
                some_index: operand_option.some_index,
                none_index: operand_option.none_index,
                function_option_type: return_name,
                function_none_index: function_option.none_index,
            },
        );
        success_type
    }

    fn require_result_return(&mut self, error_type: &Type, span: Span) {
        self.diagnostics.push(
            Diagnostic::error(
                format!(
                    "`?` requires the enclosing function to return `aster.core.Result<..., {}>`, \
                     but it returns `{}`",
                    error_type.display(),
                    self.return_type.display()
                ),
                span,
            )
            .with_help("return a `Result` so the `Error` case can propagate"),
        );
    }

    fn require_option_return(&mut self, span: Span) {
        self.diagnostics.push(
            Diagnostic::error(
                format!(
                    "`?` on an `aster.core.Option<T>` requires the enclosing function to return \
                     the official `aster.core.Option<U>`, but it returns `{}`",
                    self.return_type.display()
                ),
                span,
            )
            .with_help("return an `Option` so the `None` case can propagate"),
        );
    }

    fn internal_result_error(&mut self, name: &str, span: Span) {
        self.diagnostics.push(
            Diagnostic::error(
                format!(
                    "internal compiler error: `{name}` is not a well-formed `aster.core.Result`"
                ),
                span,
            )
            .with_help("the embedded `aster.core` standard library appears inconsistent"),
        );
    }

    fn internal_option_error(&mut self, name: &str, span: Span) {
        self.diagnostics.push(
            Diagnostic::error(
                format!(
                    "internal compiler error: `{name}` is not a well-formed `aster.core.Option`"
                ),
                span,
            )
            .with_help("the embedded `aster.core` standard library appears inconsistent"),
        );
    }

    /// Locate the `Ok` and `Error` cases of an already nominally-verified
    /// official `Result` enum, returning their positions and payload types.
    fn result_cases(&self, name: &str) -> Option<ResultCases> {
        let info = self.context.types.get(name)?;
        if info.enum_cases.len() != 2 {
            return None;
        }
        let find = |case_name: &str| {
            info.enum_cases
                .iter()
                .enumerate()
                .find(|(_, case)| case.name == case_name)
        };
        let (ok_index, ok_case) = find("Ok")?;
        let (error_index, error_case) = find("Error")?;
        if ok_case.fields.len() != 1 || error_case.fields.len() != 1 {
            return None;
        }
        Some(ResultCases {
            ok_index,
            error_index,
            success: ok_case.fields[0].1.clone(),
            error: error_case.fields[0].1.clone(),
        })
    }

    /// Locate the `Some` and `None` cases of an already nominally-verified
    /// official `Option` enum, returning their positions and `Some`'s
    /// payload type. `None` is required to carry zero fields.
    pub(super) fn option_cases(&self, name: &str) -> Option<OptionCases> {
        let info = self.context.types.get(name)?;
        if info.enum_cases.len() != 2 {
            return None;
        }
        let find = |case_name: &str| {
            info.enum_cases
                .iter()
                .enumerate()
                .find(|(_, case)| case.name == case_name)
        };
        let (some_index, some_case) = find("Some")?;
        let (none_index, none_case) = find("None")?;
        if some_case.fields.len() != 1 || !none_case.fields.is_empty() {
            return None;
        }
        Some(OptionCases {
            some_index,
            none_index,
            success: some_case.fields[0].1.clone(),
        })
    }

    pub(super) fn expression(&mut self, expression: &Expression) -> Type {
        self.expression_expected(expression, None)
    }

    pub(super) fn expression_expected(
        &mut self,
        expression: &Expression,
        expected: Option<&Type>,
    ) -> Type {
        match (&expression.kind, expected) {
            (ExpressionKind::ArrayLiteral(elements), Some(Type::Array(element)))
                if elements.is_empty() =>
            {
                self.model
                    .contextual_empty_array_elements
                    .insert(self.model_key(expression.span), element.display());
                Type::Array(element.clone())
            }
            (ExpressionKind::ArrayLiteral(elements), Some(Type::Array(element)))
                if elements.iter().any(needs_contextual_type) =>
            {
                for value in elements {
                    let actual = self.expression_expected(value, Some(element));
                    self.require_assignable_value(element, &actual, value);
                }
                Type::Array(element.clone())
            }
            (
                ExpressionKind::NewObject {
                    type_name: None,
                    arguments,
                },
                Some(expected @ (Type::Class(_) | Type::List(_) | Type::Dictionary(_, _))),
            ) => {
                let type_name = expected.display();
                self.model
                    .contextual_new_types
                    .insert(self.model_key(expression.span), type_name.clone());
                self.new_object(&type_name, arguments, expression.span)
            }
            (ExpressionKind::ObjectInitializer { creation, members }, Some(expected)) => {
                self.object_initializer(creation, members, Some(expected))
            }
            (
                ExpressionKind::Conditional {
                    condition,
                    when_true,
                    when_false,
                },
                Some(expected),
            ) => self.conditional(condition, when_true, when_false, Some(expected)),
            (
                ExpressionKind::Switch {
                    value,
                    cases,
                    default,
                },
                Some(expected),
            ) => self.switch_expression(
                value,
                cases,
                default.as_deref(),
                expression.span,
                Some(expected),
            ),
            _ => self.expression_synthesized(expression),
        }
    }

    /// `new T(args) { A = e1, B = e2 }` (LANG-008). The constructor runs
    /// first, then each member assignment in source order, against a hidden
    /// receiver bound to the new object; the object is not observable before
    /// every assignment completes. Each member is checked exactly like
    /// `receiver.Member = value`, except that an `init` accessor is allowed.
    fn object_initializer(
        &mut self,
        creation: &Expression,
        members: &[MemberInitializer],
        expected: Option<&Type>,
    ) -> Type {
        let type_ = self.expression_expected(creation, expected);
        let Type::Class(_) = &type_ else {
            if type_ != Type::Unknown {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "object initializers require a class, but this is `{}`",
                            type_.display()
                        ),
                        creation.span,
                    )
                    .with_help("assign the members in separate statements"),
                );
            }
            for member in members {
                self.expression(&member.value);
            }
            return Type::Unknown;
        };
        let mut seen = HashSet::new();
        self.scopes.push(HashMap::from([(
            INITIALIZER_RECEIVER.to_owned(),
            Binding {
                type_: type_.clone(),
                mutable: true,
                iteration_readonly: false,
                initialized: true,
                span: creation.span,
                value: None,
            },
        )]));
        for member in members {
            if !seen.insert(member.name.as_str()) {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "member `{}` is initialized more than once in this object initializer",
                            member.name
                        ),
                        member.name_span,
                    )
                    .with_help("assign each member at most once"),
                );
                self.expression(&member.value);
                continue;
            }
            self.expression(&initializer_assignment(member));
        }
        self.scopes.pop();
        type_
    }

    #[allow(clippy::too_many_lines)] // central exhaustive expression-kind dispatch
    fn expression_synthesized(&mut self, expression: &Expression) -> Type {
        match &expression.kind {
            ExpressionKind::Literal(literal) => self.literal(literal, expression.span),
            ExpressionKind::StructLiteral { type_name, fields } => {
                self.struct_literal(type_name, fields, expression.span)
            }
            ExpressionKind::ArrayLiteral(elements) => self.array_literal(elements, expression.span),
            ExpressionKind::NewArray {
                element_type,
                length,
            } => self.new_array(element_type, length, expression.span),
            ExpressionKind::NewObject {
                type_name,
                arguments,
            } => {
                if let Some(type_name) = type_name {
                    self.new_object(type_name, arguments, expression.span)
                } else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "target-typed `new()` requires an expected constructible type",
                            expression.span,
                        )
                        .with_help("use an explicit target type or write `new Type(...)`"),
                    );
                    for argument in arguments {
                        self.expression(argument);
                    }
                    Type::Unknown
                }
            }
            ExpressionKind::ObjectInitializer { creation, members } => {
                self.object_initializer(creation, members, None)
            }
            ExpressionKind::Index { array, index } => self.index(array, index, expression.span),
            ExpressionKind::Name(name) => self.name(name, expression.span),
            ExpressionKind::This => self.this_expression(expression.span),
            ExpressionKind::Base => {
                self.diagnostics.push(
                    Diagnostic::error("`base` can only be used as `base.Member`", expression.span)
                        .with_help("write `base.Method(...)` or `base.Property`"),
                );
                Type::Unknown
            }
            ExpressionKind::Member { object, name } => self.member(object, name, expression.span),
            ExpressionKind::Call {
                callee, arguments, ..
            } => self.call(callee, arguments, expression.span),
            ExpressionKind::Unary { operator, operand } => {
                // The magnitude of `long::MIN` is one larger than `long::MAX`.
                // Recognize the complete negative literal before validating the
                // positive operand, otherwise the minimum value is impossible
                // to spell even though it is representable.
                if *operator == UnaryOperator::Negate
                    && matches!(
                        &operand.kind,
                        ExpressionKind::Literal(Literal::Integer(value))
                            if value == "9223372036854775808"
                    )
                {
                    return Type::Long;
                }
                let operand_type = self.expression(operand);
                match operator {
                    UnaryOperator::Not if operand_type == Type::Bool => Type::Bool,
                    UnaryOperator::Negate
                        if operand_type.primitive().is_some_and(Primitive::is_unsigned) =>
                    {
                        self.diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "cannot negate a value of the unsigned type `{}`",
                                    operand_type.display()
                                ),
                                expression.span,
                            )
                            .with_help("cast to a signed type first, e.g. `-(long)value`"),
                        );
                        Type::Unknown
                    }
                    UnaryOperator::Negate if operand_type.is_numeric() => operand_type,
                    UnaryOperator::BitwiseNot
                        if operand_type.primitive().is_some_and(Primitive::is_integer) =>
                    {
                        operand_type
                            .primitive()
                            .and_then(crate::primitives::promote_unary)
                            .map_or(Type::Unknown, Type::from_primitive)
                    }
                    _ => {
                        self.diagnostics.push(Diagnostic::error(
                            format!(
                                "unary operator is not valid for `{}`",
                                operand_type.display()
                            ),
                            expression.span,
                        ));
                        Type::Unknown
                    }
                }
            }
            ExpressionKind::IncrementDecrement {
                operator, operand, ..
            } => self.increment_decrement(*operator, operand, expression.span),
            ExpressionKind::Try { operand } => self.try_propagate(operand, expression.span),
            ExpressionKind::Await { operand } => self.await_expression(operand, expression.span),
            ExpressionKind::Conditional {
                condition,
                when_true,
                when_false,
            } => self.conditional(condition, when_true, when_false, None),
            ExpressionKind::Switch {
                value,
                cases,
                default,
            } => self.switch_expression(value, cases, default.as_deref(), expression.span, None),
            ExpressionKind::Cast { target, operand } => self.cast(target, operand, expression.span),
            ExpressionKind::TypePattern {
                value,
                target,
                binding,
            } => self.type_pattern(value, target, binding.as_deref(), expression.span),
            ExpressionKind::TypeAs { value, target } => {
                self.type_as(value, target, expression.span)
            }
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                // `this == x` is an identity comparison, not an escape.
                let identity = matches!(operator, BinaryOperator::Equal | BinaryOperator::NotEqual);
                let left_type = if identity {
                    self.expression_receiver(left)
                } else {
                    self.expression(left)
                };
                // The right operand of `&&` runs only when the left was true
                // and that of `||` only when it was false: pattern variables
                // the left operand proves are definitely assigned there.
                let assumed = match operator {
                    BinaryOperator::LogicalAnd => super::statements::pattern_assigned(left, true),
                    BinaryOperator::LogicalOr => super::statements::pattern_assigned(left, false),
                    _ => Vec::new(),
                };
                let assumed = self.mark_assigned(&assumed);
                let right_type = if identity {
                    self.expression_receiver(right)
                } else {
                    self.expression(right)
                };
                self.unmark_assigned(&assumed);
                let result = self.binary(*operator, &left_type, &right_type, expression.span);
                if result != Type::Unknown
                    && let Some(operand) = binary_operand_type(*operator, &left_type, &right_type)
                {
                    self.model
                        .binary_operand_types
                        .insert(self.model_key(expression.span), operand.display());
                }
                result
            }
            ExpressionKind::Assignment {
                target,
                operator,
                value,
            } => self.assignment(target, *operator, value, expression.span),
            ExpressionKind::InterpolatedString { parts } => self.interpolated_string(parts),
        }
    }

    /// Validates every embedded expression and requires each to have a
    /// defined textual conversion. Always yields `string`; the concrete
    /// per-type conversion is chosen later, during lowering, from each
    /// part's own resolved type.
    fn interpolated_string(&mut self, parts: &[InterpolatedPart]) -> Type {
        for part in parts {
            let InterpolatedPart::Expression(expression) = part else {
                continue;
            };
            let type_ = self.expression(expression);
            if type_ == Type::Unknown {
                continue;
            }
            if type_ == Type::Void {
                self.diagnostics.push(Diagnostic::error(
                    "a `void` expression cannot be used in string interpolation",
                    expression.span,
                ));
                continue;
            }
            let interpolatable = matches!(
                type_.primitive(),
                Some(
                    Primitive::String
                        | Primitive::Bool
                        | Primitive::Char
                        | Primitive::SByte
                        | Primitive::Byte
                        | Primitive::Short
                        | Primitive::UShort
                        | Primitive::Int
                        | Primitive::UInt
                        | Primitive::Long
                        | Primitive::ULong
                        | Primitive::Float
                        | Primitive::Double
                )
            );
            if !interpolatable {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "type `{}` cannot be used in string interpolation",
                            type_.display()
                        ),
                        expression.span,
                    )
                    .with_help(
                        "only `string`, `bool`, `char`, integers, and floating-point types are \
                         accepted; convert the value explicitly",
                    ),
                );
            }
        }
        Type::String
    }

    fn struct_literal(
        &mut self,
        type_name: &str,
        fields: &[aster_syntax::FieldInitializer],
        span: Span,
    ) -> Type {
        let Some(info) = self.context.types.get(type_name) else {
            self.diagnostics.push(Diagnostic::error(
                format!("unknown struct `{type_name}`"),
                span,
            ));
            for field in fields {
                self.expression(&field.value);
            }
            return Type::Unknown;
        };
        if info.kind != Some(TypeKind::Struct) {
            self.diagnostics.push(
                Diagnostic::error(format!("`{type_name}` is not a struct"), span)
                    .with_help("named field literals construct struct values only"),
            );
        }
        let mut initialized = HashSet::new();
        for field in fields {
            if !initialized.insert(field.name.as_str()) {
                self.diagnostics.push(Diagnostic::error(
                    format!("field `{}` is initialized more than once", field.name),
                    field.span,
                ));
                self.expression(&field.value);
                continue;
            }
            if let Some(property) = info.properties.get(&field.name)
                && let Some(backing) = &property.auto_backing
            {
                // An auto-property is initialized by name: the literal is the
                // only construction path of a struct, so a get-only or `init`
                // property is assignable here (T-LANG-8 widens this to object
                // initializers). The backing field is never named in source.
                let expected = property.type_.clone();
                let actual = self.expression_expected(&field.value, Some(&expected));
                let accessor_visibility = property
                    .setter
                    .as_ref()
                    .or(property.getter.as_ref())
                    .map_or(Visibility::Public, |accessor| accessor.visibility);
                if accessor_visibility != Visibility::Public
                    && self.owner.as_deref() != Some(type_name)
                {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("property `{type_name}.{}` is not public", field.name),
                            field.span,
                        )
                        .with_help("construct aggregate values using public properties"),
                    );
                }
                if !initialized.insert(backing.as_str()) {
                    self.diagnostics.push(Diagnostic::error(
                        format!("field `{}` is initialized more than once", field.name),
                        field.span,
                    ));
                }
                self.require_assignable_value(&expected, &actual, &field.value);
                continue;
            }
            let Some(expected) = info.fields.get(&field.name) else {
                self.diagnostics.push(Diagnostic::error(
                    format!("struct `{type_name}` has no field `{}`", field.name),
                    field.span,
                ));
                self.expression(&field.value);
                continue;
            };
            let actual = self.expression_expected(&field.value, Some(expected));
            if info.field_visibility.get(&field.name) != Some(&Visibility::Public) {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("field `{}.{}` is not public", type_name, field.name),
                        field.span,
                    )
                    .with_help("construct aggregate values using public fields"),
                );
            }
            self.require_assignable_value(expected, &actual, &field.value);
        }
        for name in &info.field_order {
            if !initialized.contains(name.as_str()) {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "missing {} in `{type_name}` literal",
                            super::field_display(name)
                        ),
                        span,
                    )
                    .with_help("initialize every struct field explicitly"),
                );
            }
        }
        Type::User(type_name.to_owned())
    }

    /// `this` in a position that is not a field access, a method receiver or
    /// an identity comparison: it escapes (see `this_confinement`).
    fn this_expression(&mut self, span: Span) -> Type {
        if self.instance_context
            && let Some(owner) = self.owner.clone()
        {
            self.this_escapes(span);
            Type::Class(owner)
        } else {
            self.this_outside_instance(span)
        }
    }

    fn array_literal(&mut self, elements: &[Expression], span: Span) -> Type {
        let Some(first) = elements.first() else {
            self.diagnostics.push(
                Diagnostic::error(
                    "cannot infer the element type of an empty array literal",
                    span,
                )
                .with_help("use an explicit target such as `string[] values = []`, or `new T[0]`"),
            );
            return Type::Unknown;
        };
        let element_type = self.expression(first);
        for element in &elements[1..] {
            let actual = self.expression(element);
            self.require_assignable_value(&element_type, &actual, element);
        }
        Type::Array(Box::new(element_type))
    }

    fn new_array(&mut self, element: &TypeRef, length: &Expression, span: Span) -> Type {
        let element = self.resolve_local_type(element);
        let length_type = self.expression(length);
        if length_type != Type::Int && length_type != Type::Unknown {
            self.diagnostics.push(
                Diagnostic::error("array length must have type `int`", length.span)
                    .with_help("convert the length to `int`"),
            );
        }
        let constant_length = {
            let resolve = |name: &str| self.binding(name).and_then(|binding| binding.value.clone());
            evaluate(length, &resolve)
                .ok()
                .as_ref()
                .and_then(integer_value)
        };
        if constant_length.is_some_and(|value| value < 0) {
            self.diagnostics.push(Diagnostic::error(
                "array length cannot be negative",
                length.span,
            ));
        }
        if matches!(element, Type::Void | Type::Unknown) {
            return Type::Unknown;
        }
        let zero_length = constant_length == Some(0);
        if zero_length {
            self.model
                .zero_length_new_arrays
                .insert(self.model_key(span));
        }
        if !zero_length && !zero_initializable(&element, self.context, &mut HashSet::new()) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`new {}[length]` has no non-null default value",
                        element.display()
                    ),
                    span,
                )
                .with_help("use an array literal that initializes every element explicitly"),
            );
        }
        Type::Array(Box::new(element))
    }

    fn index(&mut self, array: &Expression, index: &Expression, span: Span) -> Type {
        let array_type = self.expression(array);
        let index_type = self.expression(index);
        if index_type != Type::Int && index_type != Type::Unknown {
            self.diagnostics.push(
                Diagnostic::error("index must have type `int`", index.span)
                    .with_help("convert the index to `int`"),
            );
        }
        if constant_integer(index).is_some_and(|value| value < 0) {
            self.diagnostics.push(Diagnostic::error(
                if matches!(&array_type, Type::List(_)) {
                    "list index cannot be negative"
                } else {
                    "array index cannot be negative"
                },
                index.span,
            ));
        }
        if let ExpressionKind::ArrayLiteral(elements) = &array.kind
            && let Some(index) = constant_integer(index)
            && index >= elements.len() as i128
        {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("array index {index} is outside length {}", elements.len()),
                    span,
                )
                .with_help("use an index between zero and Length - 1"),
            );
        }
        match array_type {
            Type::Array(element) => *element,
            Type::List(element) => {
                self.model.list_indexes.insert(self.model_key(span));
                *element
            }
            Type::Unknown => Type::Unknown,
            other => {
                self.diagnostics.push(Diagnostic::error(
                    format!("type `{}` cannot be indexed", other.display()),
                    array.span,
                ));
                Type::Unknown
            }
        }
    }

    fn literal(&mut self, literal: &Literal, span: Span) -> Type {
        match literal {
            Literal::Integer(text) => match classify_integer(text) {
                Some(IntegerFit::Int) => Type::Int,
                Some(IntegerFit::Long) => Type::Long,
                None => {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("integer literal `{text}` is out of range for `long`"),
                            span,
                        )
                        .with_help("`long` holds values up to 9223372036854775807"),
                    );
                    Type::Unknown
                }
            },
            Literal::Long(text) => {
                if fits_long(text) {
                    Type::Long
                } else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("integer literal `{text}L` is out of range for `long`"),
                            span,
                        )
                        .with_help("`long` holds values up to 9223372036854775807"),
                    );
                    Type::Unknown
                }
            }
            Literal::UInt(text) => match classify_unsigned(text) {
                Some(UnsignedFit::UInt) => Type::UInt,
                Some(UnsignedFit::ULong) => Type::ULong,
                None => {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("integer literal `{text}u` is out of range for `ulong`"),
                            span,
                        )
                        .with_help("`ulong` holds values up to 18446744073709551615"),
                    );
                    Type::Unknown
                }
            },
            Literal::ULong(text) => {
                if fits_ulong(text) {
                    Type::ULong
                } else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("integer literal `{text}ul` is out of range for `ulong`"),
                            span,
                        )
                        .with_help("`ulong` holds values up to 18446744073709551615"),
                    );
                    Type::Unknown
                }
            }
            Literal::Float(text) => self.floating_literal(text, Type::Float, span),
            Literal::Double(text) => self.floating_literal(text, Type::Double, span),
            Literal::Decimal(_) => Type::Decimal,
            Literal::String(_) => Type::String,
            Literal::Character(_) => Type::Char,
            Literal::Boolean(_) => Type::Bool,
        }
    }

    fn floating_literal(&mut self, text: &str, type_: Type, span: Span) -> Type {
        let finite = match type_ {
            Type::Float => text.parse::<f32>().is_ok_and(f32::is_finite),
            Type::Double => text.parse::<f64>().is_ok_and(f64::is_finite),
            _ => unreachable!("floating literal helper requires a floating type"),
        };
        if finite {
            type_
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "literal `{text}` is outside `{}` finite range",
                        type_.display()
                    ),
                    span,
                )
                .with_help("use a smaller finite literal; infinity must not arise silently"),
            );
            Type::Unknown
        }
    }

    fn cast(&mut self, target: &TypeRef, operand: &Expression, span: Span) -> Type {
        let target_type = resolve_type_readonly(target, self.context);
        let operand_type = self.expression(operand);
        if operand_type == Type::Unknown {
            return target_type;
        }
        let castable = |type_: &Type| type_.is_numeric() || *type_ == Type::Char;
        let non_integer = |type_: &Type| type_.is_float() || *type_ == Type::Decimal;
        let char_float_mix = (target_type == Type::Char && non_integer(&operand_type))
            || (operand_type == Type::Char && non_integer(&target_type));
        let invalid_integer_to_char = target_type == Type::Char
            && operand_type.primitive().is_some_and(Primitive::is_integer)
            && !self.constant_unicode_scalar(operand);
        if !castable(&target_type)
            || !castable(&operand_type)
            || char_float_mix
            || invalid_integer_to_char
        {
            let mut diagnostic = Diagnostic::error(
                format!(
                    "cannot cast `{}` to `{}`",
                    operand_type.display(),
                    target_type.display()
                ),
                span,
            );
            diagnostic = if invalid_integer_to_char {
                diagnostic.with_help(
                    "integer-to-char casts currently require a constant valid Unicode scalar",
                )
            } else if char_float_mix {
                diagnostic.with_help("cast through `int` first, e.g. `(char)(int)value`")
            } else {
                diagnostic
                    .with_help("explicit casts convert between supported numeric types and `char`")
            };
            self.diagnostics.push(diagnostic);
            return Type::Unknown;
        }
        target_type
    }

    fn constant_unicode_scalar(&self, expression: &Expression) -> bool {
        let resolve = |name: &str| self.binding(name).and_then(|binding| binding.value.clone());
        evaluate(expression, &resolve)
            .ok()
            .and_then(|value| integer_value(&value))
            .and_then(|value| u32::try_from(value).ok())
            .and_then(char::from_u32)
            .is_some()
    }

    fn increment_decrement(
        &mut self,
        operator: IncrementOperator,
        operand: &Expression,
        span: Span,
    ) -> Type {
        let symbol = match operator {
            IncrementOperator::Increment => "++",
            IncrementOperator::Decrement => "--",
        };
        if matches!(operand.kind, ExpressionKind::Index { .. }) {
            let operand_type = self.expression(operand);
            if self
                .model
                .list_indexes
                .contains(&self.model_key(operand.span))
            {
                if operand_type.is_numeric() || operand_type == Type::Unknown {
                    return operand_type;
                }
                self.diagnostics.push(Diagnostic::error(
                    format!("`{symbol}` requires a numeric list element"),
                    span,
                ));
                return Type::Unknown;
            }
        }
        let ExpressionKind::Name(name) = &operand.kind else {
            let operand_type = self.expression(operand);
            if operand_type != Type::Unknown {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("the operand of `{symbol}` must be a mutable variable"),
                        span,
                    )
                    .with_help(format!(
                        "`{symbol}` cannot be applied to literals or temporary expression results"
                    )),
                );
            }
            return Type::Unknown;
        };
        let Some(binding) = self.binding(name).cloned() else {
            self.diagnostics.push(
                Diagnostic::error(format!("unknown name `{name}`"), operand.span)
                    .with_help("declare the name before using it"),
            );
            return Type::Unknown;
        };
        if !binding.initialized {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("variable `{name}` is used before initialization"),
                    operand.span,
                )
                .with_help("assign a value before reading the variable"),
            );
        }
        if !binding.mutable {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("cannot apply `{symbol}` to constant `{name}`"),
                    span,
                )
                .with_help("apply the operator to a mutable variable instead"),
            );
            return binding.type_;
        }
        if binding.type_.is_numeric() || binding.type_ == Type::Unknown {
            binding.type_
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("`{symbol}` is not valid for `{}`", binding.type_.display()),
                    span,
                )
                .with_help("apply the operator to a numeric variable"),
            );
            Type::Unknown
        }
    }

    fn conditional(
        &mut self,
        condition: &Expression,
        when_true: &Expression,
        when_false: &Expression,
        expected: Option<&Type>,
    ) -> Type {
        let condition_type = self.expression(condition);
        if condition_type != Type::Bool && condition_type != Type::Unknown {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`?:` condition must be `bool`, found `{}`",
                        condition_type.display()
                    ),
                    condition.span,
                )
                .with_help("use a boolean expression before `?`"),
            );
        }
        let true_type = self.expression_expected(when_true, expected);
        let false_type = self.expression_expected(when_false, expected);
        if let Some(expected) = expected {
            self.require_assignable_value(expected, &true_type, when_true);
            self.require_assignable_value(expected, &false_type, when_false);
            return expected.clone();
        }
        if true_type == Type::Unknown || false_type == Type::Unknown {
            return Type::Unknown;
        }
        if true_type == Type::Void || false_type == Type::Void {
            self.diagnostics.push(Diagnostic::error(
                "`?:` branches must produce a value",
                when_true.span,
            ));
            return Type::Unknown;
        }
        if let Some(promoted) = promoted_numeric(&true_type, &false_type) {
            promoted
        } else if self.compatible(&true_type, &false_type) {
            true_type
        } else if self.compatible(&false_type, &true_type) {
            false_type
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`?:` branches have incompatible types `{}` and `{}`",
                        true_type.display(),
                        false_type.display()
                    ),
                    when_false.span,
                )
                .with_help("give both branches a compatible type"),
            );
            Type::Unknown
        }
    }

    /// Whether the class named `class` declares or inherits `interface`.
    fn class_implements(&self, class: &str, interface: &str) -> bool {
        self.context.types.get(class).is_some_and(|info| {
            info.implemented_interfaces
                .iter()
                .any(|implemented| implemented == interface)
        })
    }

    /// Whether no class can derive from `class`.
    fn class_is_sealed(&self, class: &str) -> bool {
        self.context
            .types
            .get(class)
            .is_some_and(|info| info.is_sealed)
    }

    /// Relate the static type of a value to the target of `is` / `as` (object
    /// model B1, sections 7 and 10.1). A class subject may be tested against a
    /// related class (derived: a real test; equal or base: always true) or an
    /// interface (statically implemented: always true; otherwise a run-time
    /// search of the object descriptor, unless the class is sealed and the
    /// test can never succeed). An interface subject may be tested against a
    /// class that implements it or may have a subclass that does, or against
    /// another interface. Returns the static type of the narrowed value, or
    /// `Unknown` after reporting an error.
    #[allow(clippy::too_many_lines)]
    fn runtime_type_relation(
        &mut self,
        subject: &Type,
        target_type: &Type,
        target: &aster_syntax::TypeRef,
        value: &Expression,
        span: Span,
        operator: &str,
    ) -> Type {
        let never = if operator == "is" {
            "pattern can never match".to_owned()
        } else {
            "`as` can never succeed".to_owned()
        };
        match (subject, target_type) {
            (Type::Unknown, _) | (_, Type::Unknown) => Type::Unknown,
            (Type::Enum(name), _) if self.is_official_option(name) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("unwrapping an `Option<T>` with `{operator}` is not supported yet"),
                        value.span,
                    )
                    .with_help("use a `switch` with `Some(value)` and `None` cases"),
                );
                Type::Unknown
            }
            (Type::Class(subject_class), Type::Class(target_class)) => {
                if self.derives_from(target_class, subject_class)
                    || self.derives_from(subject_class, target_class)
                {
                    target_type.clone()
                } else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "{never}: `{subject_class}` and `{target_class}` are unrelated classes"
                            ),
                            span,
                        )
                        .with_help("the target class must derive from, or be a base of, the class of the value"),
                    );
                    Type::Unknown
                }
            }
            (Type::Class(subject_class), Type::Interface(interface)) => {
                if self.class_implements(subject_class, interface)
                    || !self.class_is_sealed(subject_class)
                {
                    target_type.clone()
                } else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "{never}: sealed class `{subject_class}` does not implement interface `{interface}`"
                            ),
                            span,
                        )
                        .with_help("a sealed class has no subclass that could implement the interface"),
                    );
                    Type::Unknown
                }
            }
            (Type::Interface(interface), Type::Class(target_class)) => {
                if self.class_implements(target_class, interface)
                    || !self.class_is_sealed(target_class)
                {
                    target_type.clone()
                } else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "{never}: sealed class `{target_class}` does not implement interface `{interface}`"
                            ),
                            span,
                        )
                        .with_help("a sealed class has no subclass that could implement the interface"),
                    );
                    Type::Unknown
                }
            }
            (Type::Interface(_), Type::Interface(_)) => target_type.clone(),
            (Type::Class(_) | Type::Interface(_), other) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "a type pattern target must be a class, found `{}`",
                            other.display()
                        ),
                        target.span,
                    )
                    .with_help("name a class or an interface related to the type of the value"),
                );
                Type::Unknown
            }
            (other, _) => {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "a type pattern requires a class-typed value, found `{}`",
                            other.display()
                        ),
                        value.span,
                    )
                    .with_help("`is` and `as` test the run-time type of a class reference or an interface value"),
                );
                Type::Unknown
            }
        }
    }

    /// `value is Type [name]`: a run-time class or interface test (see
    /// [`Self::runtime_type_relation`]). The binding is declared unassigned;
    /// the definite-assignment rules mark it assigned where the test succeeded.
    fn type_pattern(
        &mut self,
        value: &Expression,
        target: &aster_syntax::TypeRef,
        binding: Option<&aster_syntax::PatternBinding>,
        span: Span,
    ) -> Type {
        let subject = if binding.is_some() {
            self.expression(value)
        } else {
            self.expression_receiver(value)
        };
        let target_type = self.resolve_local_type(target);
        let bound = self.runtime_type_relation(&subject, &target_type, target, value, span, "is");
        if let Some(binding) = binding {
            // Same conservative rule as for an ordinary local: a class
            // reference cannot cross the single `await` in this version.
            if bound != Type::Unknown && self.async_state == super::AsyncAnalysisState::BeforeAwait
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "a reference-typed local cannot be declared before `await` in this version",
                        binding.span,
                    )
                    .with_help("only scalar locals may cross an `await`"),
                );
            }
            self.declare(
                &binding.name,
                Binding {
                    type_: bound,
                    mutable: true,
                    iteration_readonly: false,
                    initialized: false,
                    span: binding.span,
                    value: None,
                },
            );
        }
        Type::Bool
    }

    /// `value as Type`: a checked narrowing that yields the official
    /// `Option<Type>` (`Some` where the run-time type matches, `None`
    /// otherwise). The relation rules are those of `is`; an impossible `as` is
    /// an error and a statically trivial one is allowed.
    fn type_as(&mut self, value: &Expression, target: &aster_syntax::TypeRef, span: Span) -> Type {
        let subject = self.expression(value);
        let target_type = self.resolve_local_type(target);
        if self.runtime_type_relation(&subject, &target_type, target, value, span, "as")
            == Type::Unknown
        {
            return Type::Unknown;
        }
        if self.context.official_option.is_none() {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`as {}` requires `aster.core.Option<{}>`",
                        target.name, target.name
                    ),
                    span,
                )
                .with_help("add `using aster.core;` to this file"),
            );
            return Type::Unknown;
        }
        let option_name = crate::standard_library::option_specialization_name(&target.name);
        let Some(cases) = self
            .option_cases(&option_name)
            .filter(|_| self.context.types.contains_key(&option_name))
        else {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`as {}` requires the `Option<{}>` specialization, which was not generated",
                        target.name, target.name
                    ),
                    span,
                )
                .with_help("this indicates an internal compiler error, not a source mistake"),
            );
            return Type::Unknown;
        };
        self.model.type_as.insert(
            self.model_key(span),
            ResolvedTypeAs {
                option_type: option_name.clone(),
                some_index: cases.some_index,
                none_index: cases.none_index,
            },
        );
        Type::Enum(option_name)
    }

    /// Mark pattern variables definitely assigned; returns the ones that were
    /// unassigned (and so must be restored by `unmark_assigned`).
    pub(super) fn mark_assigned(&mut self, names: &[String]) -> Vec<String> {
        let mut changed = Vec::new();
        for name in names {
            if let Some(binding) = self.binding_mut(name)
                && !binding.initialized
            {
                binding.initialized = true;
                changed.push(name.clone());
            }
        }
        changed
    }

    pub(super) fn unmark_assigned(&mut self, names: &[String]) {
        for name in names {
            if let Some(binding) = self.binding_mut(name) {
                binding.initialized = false;
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn switch_expression(
        &mut self,
        value: &Expression,
        cases: &[aster_syntax::SwitchExpressionCase],
        default: Option<&Expression>,
        span: Span,
        expected: Option<&Type>,
    ) -> Type {
        let selected = self.expression(value);
        if super::statements::is_scalar_switch_subject(&selected) {
            return self.scalar_switch_expression(&selected, cases, default, span, expected);
        }
        let Type::Enum(enum_name) = selected else {
            if selected != Type::Unknown {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`switch` expression requires an enum, integer, `char` or `string` value, found `{}`",
                            selected.display()
                        ),
                        value.span,
                    )
                    .with_help("select an enum, integer, `char` or `string` value"),
                );
            }
            for case in cases {
                self.expression_expected(&case.value, expected);
            }
            if let Some(default) = default {
                self.expression_expected(default, expected);
            }
            return Type::Unknown;
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
        let mut result_type = None;
        for case in cases {
            // A guarded arm can fall through, so it never counts as covering
            // its case; it is still checked against the unguarded arms.
            let mut scratch = covered.clone();
            let info = self.resolve_switch_pattern(
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
            );
            self.scopes.push(HashMap::new());
            if let Some(info) = info {
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
            }
            let assigned = self.analyze_guard(case.guard.as_ref());
            self.mark_assigned(&assigned);
            let arm_type = self.expression_expected(&case.value, expected);
            if let Some(expected) = expected {
                self.require_assignable_value(expected, &arm_type, &case.value);
            }
            self.scopes.pop();
            result_type = Some(self.switch_result_type(result_type, arm_type, case.value.span));
        }
        if let Some(default) = default {
            self.validate_switch_coverage(&enum_cases, &covered, Some(default.span), span);
            let arm_type = self.expression_expected(default, expected);
            if let Some(expected) = expected {
                self.require_assignable_value(expected, &arm_type, default);
            }
            result_type = Some(self.switch_result_type(result_type, arm_type, default.span));
        } else {
            self.validate_switch_coverage(&enum_cases, &covered, None, span);
        }
        let result_type = result_type.unwrap_or_else(|| {
            self.diagnostics.push(Diagnostic::error(
                "a switch expression must contain at least one result arm",
                span,
            ));
            Type::Unknown
        });
        let result_type = expected.cloned().unwrap_or(result_type);
        if result_type != Type::Unknown {
            self.model
                .switch_expression_types
                .insert(self.model_key(span), result_type.display());
        }
        result_type
    }

    /// Integer, `char` and `string` switch expression: constant labels, no
    /// exhaustiveness, so `default` is mandatory.
    fn scalar_switch_expression(
        &mut self,
        selected: &Type,
        cases: &[aster_syntax::SwitchExpressionCase],
        default: Option<&Expression>,
        span: Span,
        expected: Option<&Type>,
    ) -> Type {
        let mut seen = HashSet::new();
        let mut result_type = None;
        for case in cases {
            // A guarded arm can fall through, so its label is only checked
            // against earlier unguarded arms and is not recorded.
            if case.guard.is_some() {
                let mut scratch = seen.clone();
                self.check_scalar_label(selected, case.label.as_ref(), case.span, &mut scratch);
            } else {
                self.check_scalar_label(selected, case.label.as_ref(), case.span, &mut seen);
            }
            self.scopes.push(HashMap::new());
            let assigned = self.analyze_guard(case.guard.as_ref());
            self.mark_assigned(&assigned);
            let arm_type = self.expression_expected(&case.value, expected);
            if let Some(expected) = expected {
                self.require_assignable_value(expected, &arm_type, &case.value);
            }
            self.scopes.pop();
            result_type = Some(self.switch_result_type(result_type, arm_type, case.value.span));
        }
        if let Some(default) = default {
            let arm_type = self.expression_expected(default, expected);
            if let Some(expected) = expected {
                self.require_assignable_value(expected, &arm_type, default);
            }
            result_type = Some(self.switch_result_type(result_type, arm_type, default.span));
        } else {
            self.require_scalar_default(selected, span);
        }
        let result_type = result_type.unwrap_or_else(|| {
            self.diagnostics.push(Diagnostic::error(
                "a switch expression must contain at least one result arm",
                span,
            ));
            Type::Unknown
        });
        let result_type = expected.cloned().unwrap_or(result_type);
        if result_type != Type::Unknown {
            self.model
                .switch_expression_types
                .insert(self.model_key(span), result_type.display());
        }
        result_type
    }

    fn switch_result_type(&mut self, current: Option<Type>, next: Type, span: Span) -> Type {
        if next == Type::Unknown {
            return Type::Unknown;
        }
        if next == Type::Void {
            self.diagnostics.push(Diagnostic::error(
                "switch expression arms must produce a value",
                span,
            ));
            return Type::Unknown;
        }
        let Some(current) = current else {
            return next;
        };
        if current == Type::Unknown {
            return Type::Unknown;
        }
        if let Some(promoted) = promoted_numeric(&current, &next) {
            promoted
        } else if self.compatible(&current, &next) {
            current
        } else if self.compatible(&next, &current) {
            next
        } else {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "switch expression arms have incompatible types `{}` and `{}`",
                        current.display(),
                        next.display()
                    ),
                    span,
                )
                .with_help("give every arm a compatible result type"),
            );
            Type::Unknown
        }
    }

    fn binary(&mut self, operator: BinaryOperator, left: &Type, right: &Type, span: Span) -> Type {
        use BinaryOperator::{
            Add, BitwiseAnd, BitwiseOr, BitwiseXor, Divide, Equal, Greater, GreaterEqual, Less,
            LessEqual, LogicalAnd, LogicalOr, Multiply, NotEqual, Remainder, ShiftLeft, ShiftRight,
            Subtract, UnsignedShiftRight,
        };
        if *left == Type::Unknown || *right == Type::Unknown {
            return Type::Unknown;
        }
        if operator == Add
            && (*left == Type::String || *right == Type::String)
            && !(*left == Type::String && *right == Type::String)
        {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "string concatenation requires two `string` operands, found `{}` and `{}`",
                        left.display(),
                        right.display()
                    ),
                    span,
                )
                .with_help(
                    "convert the value explicitly before concatenating; implicit textual conversion is not implemented",
                ),
            );
            return Type::Unknown;
        }
        match operator {
            LogicalAnd | LogicalOr if *left == Type::Bool && *right == Type::Bool => Type::Bool,
            Equal | NotEqual if self.equality_compatible(left, right, &mut HashSet::new()) => {
                Type::Bool
            }
            Equal | NotEqual | Less | LessEqual | Greater | GreaterEqual
                if left.is_numeric() && right.is_numeric() =>
            {
                promoted_numeric(left, right)
                    .map_or_else(|| self.no_common_type(left, right, span), |_| Type::Bool)
            }
            Equal | NotEqual if compatible(left, right) || compatible(right, left) => Type::Bool,
            Add if *left == Type::String && *right == Type::String => Type::String,
            Add | Subtract | Multiply | Divide | Remainder
                if left.is_numeric() && right.is_numeric() =>
            {
                match promoted_numeric(left, right) {
                    Some(promoted) => promoted,
                    None => self.no_common_type(left, right, span),
                }
            }
            // Eager Boolean operators: both operands are always evaluated.
            BitwiseAnd | BitwiseOr | BitwiseXor if *left == Type::Bool && *right == Type::Bool => {
                Type::Bool
            }
            BitwiseAnd | BitwiseOr | BitwiseXor if is_integer(left) && is_integer(right) => {
                match promoted_numeric(left, right) {
                    Some(promoted) => promoted,
                    None => self.no_common_type(left, right, span),
                }
            }
            ShiftLeft | ShiftRight | UnsignedShiftRight
                if is_integer(left) && is_integer(right) =>
            {
                if !is_shift_count(right) {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "shift count of type `{}` does not convert to `int`",
                                right.display()
                            ),
                            span,
                        )
                        .with_help("cast the count explicitly, e.g. `(int)count`"),
                    );
                    return Type::Unknown;
                }
                left.primitive()
                    .and_then(crate::primitives::promote_unary)
                    .map_or(Type::Unknown, Type::from_primitive)
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "binary operator is not valid for `{}` and `{}`",
                            left.display(),
                            right.display()
                        ),
                        span,
                    )
                    .with_help("use operands with compatible types"),
                );
                Type::Unknown
            }
        }
    }

    fn equality_compatible(
        &self,
        left: &Type,
        right: &Type,
        visiting: &mut HashSet<String>,
    ) -> bool {
        match (left, right) {
            (Type::User(left_name), Type::User(right_name)) if left_name == right_name => {
                if !visiting.insert(left_name.clone()) {
                    return false;
                }
                let comparable = self.context.types.get(left_name).is_some_and(|info| {
                    info.fields
                        .values()
                        .all(|field| self.equality_compatible(field, field, visiting))
                });
                visiting.remove(left_name);
                comparable
            }
            (Type::Array(left), Type::Array(right)) => left == right,
            // Identity comparison across a declared inheritance chain.
            (Type::Class(left), Type::Class(right)) => {
                left == right || self.derives_from(left, right) || self.derives_from(right, left)
            }
            (Type::Interface(_), Type::Interface(_)) => true,
            (Type::Enum(left_name), Type::Enum(right_name)) if left_name == right_name => {
                if !visiting.insert(left_name.clone()) {
                    return false;
                }
                let comparable = self.context.types.get(left_name).is_some_and(|info| {
                    info.enum_cases.iter().all(|case| {
                        case.fields
                            .iter()
                            .all(|(_, field)| self.equality_compatible(field, field, visiting))
                    })
                });
                visiting.remove(left_name);
                comparable
            }
            _ => compatible(left, right) || compatible(right, left),
        }
    }

    fn no_common_type(&mut self, left: &Type, right: &Type, span: Span) -> Type {
        self.diagnostics.push(
            Diagnostic::error(
                format!(
                    "`{}` and `{}` have no implicit common type",
                    left.display(),
                    right.display()
                ),
                span,
            )
            .with_help("cast one operand explicitly, e.g. `(long)value`"),
        );
        Type::Unknown
    }

    /// Whether `expression` names a real, observably-mutable memory location
    /// rather than a temporary value. A local/parameter/`this` field, any
    /// array/list index (arrays and lists are always heap references, so
    /// indexing one is valid regardless of how the reference itself was
    /// obtained -- unchanged, existing behavior), and a field reached
    /// through a class/interface reference (a class is a shared reference:
    /// mutating a field through *any* reference to it, even one just
    /// returned by a call, is observable) all qualify. A field of a
    /// struct-typed (`Type::User`) base only qualifies when that base is
    /// itself assignable this same way; a bare call, literal, or other pure
    /// rvalue is not, because mutating a copy about to be discarded has no
    /// observable effect (see `mir_lowering::places`'s `place()`, which
    /// materializes such reads into a temporary rather than writing back to
    /// anything).
    fn is_assignable_place(&mut self, expression: &Expression) -> bool {
        match &expression.kind {
            ExpressionKind::Name(name) => self
                .binding(name)
                .is_none_or(|binding| !binding.iteration_readonly),
            ExpressionKind::This | ExpressionKind::Index { .. } => true,
            ExpressionKind::Member { object, .. } => {
                matches!(
                    self.expression_receiver(object),
                    Type::Class(_) | Type::Interface(_)
                ) || self.is_assignable_place(object)
            }
            _ => false,
        }
    }

    /// A foreach element is a copied value. A struct field reached through it
    /// must not become a write-back alias, while a class member remains an
    /// observable mutation through a copied reference.
    fn foreach_readonly_binding(&mut self, expression: &Expression) -> Option<String> {
        match &expression.kind {
            ExpressionKind::Name(name) => self
                .binding(name)
                .is_some_and(|binding| binding.iteration_readonly)
                .then(|| name.clone()),
            ExpressionKind::Member { object, .. }
                if matches!(self.expression(object), Type::User(_)) =>
            {
                self.foreach_readonly_binding(object)
            }
            _ => None,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn assignment(
        &mut self,
        target: &Expression,
        operator: AssignmentOperator,
        value: &Expression,
        span: Span,
    ) -> Type {
        let contextual_target = if operator == AssignmentOperator::Assign {
            match &target.kind {
                ExpressionKind::Name(name) => self
                    .binding(name)
                    .map(|binding| binding.type_.clone())
                    .or_else(|| {
                        self.owner.as_ref().and_then(|owner| {
                            self.context
                                .types
                                .get(owner)
                                .and_then(|info| info.properties.get(name))
                                .map(|property| property.type_.clone())
                        })
                    }),
                ExpressionKind::Index { array, .. } => match self.expression(array) {
                    Type::Array(element) | Type::List(element) => Some(*element),
                    _ => None,
                },
                ExpressionKind::Member { object, name } => {
                    let object_type = if matches!(object.kind, ExpressionKind::This) {
                        self.owner.as_ref().map_or(Type::Unknown, |owner| {
                            match self.context.types.get(owner).and_then(|info| info.kind) {
                                Some(TypeKind::Class) => Type::Class(owner.clone()),
                                Some(TypeKind::Interface) => Type::Interface(owner.clone()),
                                Some(TypeKind::Struct) => Type::User(owner.clone()),
                                _ => Type::Unknown,
                            }
                        })
                    } else if matches!(object.kind, ExpressionKind::Base) {
                        self.base_class_of_owner()
                            .map_or(Type::Unknown, Type::Class)
                    } else {
                        self.expression(object)
                    };
                    match object_type {
                        Type::User(owner) | Type::Class(owner) | Type::Interface(owner) => {
                            self.context.types.get(&owner).and_then(|info| {
                                info.fields.get(name).cloned().or_else(|| {
                                    info.properties
                                        .get(name)
                                        .map(|property| property.type_.clone())
                                })
                            })
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        } else {
            None
        };
        let value_type = self.expression_expected(value, contextual_target.as_ref());
        if let ExpressionKind::Name(name) = &target.kind
            && self.instance_context
            && let Some(owner) = self.owner.clone()
            && let Some(property) = self
                .context
                .types
                .get(&owner)
                .and_then(|info| info.properties.get(name))
                .cloned()
        {
            return self.assign_property(
                &owner,
                name,
                property,
                operator,
                &value_type,
                value,
                span,
                target.span,
            );
        }
        if let ExpressionKind::Member { object, name } = &target.kind
            && matches!(object.kind, ExpressionKind::This)
            && self.field_names.contains(name)
        {
            let field_type = self
                .binding(name)
                .map_or(Type::Unknown, |binding| binding.type_.clone());
            if operator != AssignmentOperator::Assign {
                self.name(name, target.span);
            }
            let result = self.assignment_types(operator, field_type, &value_type, value, span);
            if let Some(binding) = self.binding_mut(name) {
                binding.initialized = true;
            }
            return result;
        }
        if let ExpressionKind::Member { object, name } = &target.kind
            && matches!(object.kind, ExpressionKind::This)
            && let Some(owner) = self.owner.clone()
            && let Some(property) = self
                .context
                .types
                .get(&owner)
                .and_then(|info| info.properties.get(name))
                .cloned()
        {
            return self.assign_property(
                &owner,
                name,
                property,
                operator,
                &value_type,
                value,
                span,
                target.span,
            );
        }
        if let ExpressionKind::Member { object, name } = &target.kind
            && matches!(object.kind, ExpressionKind::Base)
        {
            let Some(base) = self.base_class_of_owner() else {
                self.base_receiver(object.span);
                return Type::Unknown;
            };
            if let Some(property) = self
                .context
                .types
                .get(&base)
                .and_then(|info| info.properties.get(name))
                .cloned()
            {
                if property
                    .setter
                    .as_ref()
                    .is_some_and(|setter| setter.dispatch == MemberDispatch::Abstract)
                {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("`base.{name}` is abstract and has no implementation to call"),
                            target.span,
                        )
                        .with_help("write a concrete member, or implement it in the base class"),
                    );
                }
                self.base_access = true;
                let result = self.assign_property(
                    &base,
                    name,
                    property,
                    operator,
                    &value_type,
                    value,
                    span,
                    target.span,
                );
                self.base_access = false;
                return result;
            }
        }
        if let ExpressionKind::Member { object, name } = &target.kind {
            let object_type = self.expression_receiver(object);
            let struct_receiver = matches!(object_type, Type::User(_));
            if let Type::Class(type_name) | Type::User(type_name) = object_type
                && let Some(property) = self
                    .context
                    .types
                    .get(&type_name)
                    .and_then(|info| info.properties.get(name))
                    .cloned()
            {
                if struct_receiver {
                    if let Some(readonly) = self.foreach_readonly_binding(target) {
                        self.diagnostics.push(
                            Diagnostic::error(
                                format!("foreach variable `{readonly}` is read-only"),
                                target.span,
                            )
                            .with_help("use an indexed loop to modify array elements"),
                        );
                        return Type::Unknown;
                    }
                    if !self.is_assignable_place(target) {
                        self.diagnostics.push(
                            Diagnostic::error(
                                "cannot assign to a field of a temporary value",
                                target.span,
                            )
                            .with_help(
                                "store the value in a variable first, then assign to its field",
                            ),
                        );
                        return Type::Unknown;
                    }
                }
                let Some(setter) = property.setter.clone() else {
                    self.report_read_only_property(&type_name, name, &property, target.span);
                    return Type::Unknown;
                };
                if property.init_only
                    && !matches!(&object.kind, ExpressionKind::Name(receiver)
                        if receiver == INITIALIZER_RECEIVER)
                {
                    self.report_init_assignment(&type_name, name, target.span);
                    return Type::Unknown;
                }
                let declared_in = setter
                    .key
                    .owner
                    .clone()
                    .unwrap_or_else(|| type_name.clone());
                self.check_member_access(
                    setter.visibility,
                    &declared_in,
                    &type_name,
                    &format!("setter for property `{type_name}.{name}`"),
                    target.span,
                );
                if operator != AssignmentOperator::Assign && property.getter.is_none() {
                    self.diagnostics.push(Diagnostic::error(
                        format!("compound assignment requires getter for `{type_name}.{name}`"),
                        target.span,
                    ));
                    return Type::Unknown;
                }
                let result = self.assignment_types(
                    operator,
                    property.type_.clone(),
                    &value_type,
                    value,
                    span,
                );
                if let Some(backing) = property.direct_backing() {
                    self.model
                        .auto_property_fields
                        .insert(self.model_key(target.span), backing.to_owned());
                    return result;
                }
                self.model.property_assignments.insert(
                    self.model_key(span),
                    ResolvedPropertyAssignment {
                        getter: property.getter.map(|getter| getter.key),
                        setter: setter.key,
                    },
                );
                return result;
            }
        }
        if let ExpressionKind::Member { object, name } = &target.kind
            && name == "Length"
        {
            match self.expression(object) {
                Type::Array(_) => {
                    self.diagnostics.push(
                        Diagnostic::error("array Length is read-only", target.span)
                            .with_help("create an array with the required fixed length"),
                    );
                    return Type::Unknown;
                }
                Type::String => {
                    self.diagnostics.push(
                        Diagnostic::error("string Length is read-only", target.span)
                            .with_help("assign a different string value instead"),
                    );
                    return Type::Unknown;
                }
                Type::List(_) => {
                    self.diagnostics.push(
                        Diagnostic::error("list Length is read-only", target.span).with_help(
                            "Length reflects the list's element count and cannot be set directly",
                        ),
                    );
                    return Type::Unknown;
                }
                Type::Dictionary(_, _) => {
                    self.diagnostics.push(Diagnostic::error(
                        "dictionary Length is read-only",
                        target.span,
                    ));
                    return Type::Unknown;
                }
                _ => {}
            }
        }
        if let ExpressionKind::Member { object, name } = &target.kind
            && name == "Capacity"
        {
            match self.expression(object) {
                Type::List(_) => {
                    self.diagnostics.push(
                        Diagnostic::error("list Capacity is read-only", target.span)
                            .with_help("call EnsureCapacity to request additional list capacity"),
                    );
                    return Type::Unknown;
                }
                Type::Dictionary(_, _) => {
                    self.diagnostics.push(
                        Diagnostic::error("dictionary Capacity is read-only", target.span)
                            .with_help(
                                "call EnsureCapacity to request additional dictionary capacity",
                            ),
                    );
                    return Type::Unknown;
                }
                _ => {}
            }
        }
        if let Some(name) = self.foreach_readonly_binding(target) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("foreach variable `{name}` is read-only"),
                    target.span,
                )
                .with_help("use an indexed loop to modify array elements"),
            );
            return Type::Unknown;
        }
        let ExpressionKind::Name(name) = &target.kind else {
            if !self.is_assignable_place(target) {
                self.diagnostics.push(
                    Diagnostic::error("cannot assign to a field of a temporary value", target.span)
                        .with_help("store the value in a variable first, then assign to its field"),
                );
                return Type::Unknown;
            }
            let target_type = self.expression(target);
            return self.assignment_types(operator, target_type, &value_type, value, span);
        };
        let Some(binding) = self.binding(name).cloned() else {
            self.diagnostics.push(Diagnostic::error(
                format!("unknown name `{name}`"),
                target.span,
            ));
            return Type::Unknown;
        };
        if !binding.mutable {
            self.diagnostics.push(
                Diagnostic::error(format!("cannot assign to constant `{name}`"), target.span)
                    .with_help("assign to a mutable variable instead"),
            );
        }
        if operator != AssignmentOperator::Assign {
            self.note_this_field_read(name);
        }
        let result =
            self.assignment_types(operator, binding.type_.clone(), &value_type, value, span);
        if let Some(binding) = self.binding_mut(name) {
            binding.initialized = true;
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn assign_property(
        &mut self,
        owner: &str,
        name: &str,
        property: PropertyInfo,
        operator: AssignmentOperator,
        value_type: &Type,
        value: &Expression,
        span: Span,
        target_span: Span,
    ) -> Type {
        // A `this`/bare-name write inside a constructor of the declaring class
        // reaches the backing field of an auto-property directly, even when
        // the property has no setter (get-only) or an `init` setter.
        let in_declaring_constructor = self.constructor
            && !self.base_access
            && self.owner.as_deref() == Some(property.declared_in.as_str());
        let Some(setter) = property.setter.clone() else {
            if !in_declaring_constructor || property.auto_backing.is_none() {
                self.report_read_only_property(owner, name, &property, target_span);
                return Type::Unknown;
            }
            return self.assign_backing_field(
                name,
                &property,
                operator,
                value_type,
                value,
                span,
                target_span,
            );
        };
        if property.init_only && !self.constructor {
            self.report_init_assignment(owner, name, target_span);
            return Type::Unknown;
        }
        if operator != AssignmentOperator::Assign && property.getter.is_none() {
            self.diagnostics.push(Diagnostic::error(
                format!("compound assignment requires getter for `{owner}.{name}`"),
                target_span,
            ));
            return Type::Unknown;
        }
        if property.direct_backing().is_some() && !self.base_access {
            return self.assign_backing_field(
                name,
                &property,
                operator,
                value_type,
                value,
                span,
                target_span,
            );
        }
        let result =
            self.assignment_types(operator, property.type_.clone(), value_type, value, span);
        self.note_this_call(&setter.key, target_span);
        if operator != AssignmentOperator::Assign
            && let Some(getter) = &property.getter
        {
            self.note_this_call(&getter.key, target_span);
        }
        self.model.property_assignments.insert(
            self.model_key(span),
            ResolvedPropertyAssignment {
                getter: property.getter.map(|getter| getter.key),
                setter: setter.key,
            },
        );
        result
    }

    /// A write of `this.<name>` / `<name>` to an auto-property that HIR lowers
    /// to its backing field: a field assignment for definite assignment and
    /// this-confinement, with no accessor call.
    #[allow(clippy::too_many_arguments)]
    fn assign_backing_field(
        &mut self,
        name: &str,
        property: &PropertyInfo,
        operator: AssignmentOperator,
        value_type: &Type,
        value: &Expression,
        span: Span,
        target_span: Span,
    ) -> Type {
        let backing = property
            .auto_backing
            .clone()
            .expect("direct writes target auto-properties");
        if operator != AssignmentOperator::Assign {
            let Some(getter) = &property.getter else {
                self.diagnostics.push(Diagnostic::error(
                    format!("compound assignment requires getter for `{name}`"),
                    target_span,
                ));
                return Type::Unknown;
            };
            let declared_here = self.owner.as_deref() == Some(property.declared_in.as_str());
            self.note_auto_property_read(&backing, declared_here, &getter.key, name, target_span);
        }
        let result =
            self.assignment_types(operator, property.type_.clone(), value_type, value, span);
        if let Some(binding) = self.binding_mut(&backing) {
            binding.initialized = true;
        }
        self.model
            .auto_property_fields
            .insert(self.model_key(target_span), backing);
        result
    }

    /// A write to a property without a setter. A get-only auto-property is
    /// assignable only in a constructor of its declaring class.
    fn report_read_only_property(
        &mut self,
        owner: &str,
        name: &str,
        property: &PropertyInfo,
        span: Span,
    ) {
        if property.auto_backing.is_some() {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("get-only auto-property `{owner}.{name}` cannot be assigned here"),
                    span,
                )
                .with_help(format!(
                    "assign it in a constructor of `{}`, or declare `set` or `init`",
                    property.declared_in
                )),
            );
        } else {
            self.diagnostics.push(
                Diagnostic::error(format!("property `{owner}.{name}` is read-only"), span)
                    .with_help("add a setter or assign to a mutable field inside the class"),
            );
        }
    }

    /// A write to an `init` property outside a constructor or an object
    /// initializer (the two permitted contexts, checked by the callers).
    fn report_init_assignment(&mut self, owner: &str, name: &str, span: Span) {
        self.diagnostics.push(
            Diagnostic::error(
                format!(
                    "init-only property `{owner}.{name}` can be assigned only in a constructor or an object initializer"
                ),
                span,
            )
            .with_help(
                "assign it from a constructor, or in `new Type(...) { Name = value }`",
            ),
        );
    }

    fn assignment_types(
        &mut self,
        operator: AssignmentOperator,
        target: Type,
        value: &Type,
        value_expression: &Expression,
        span: Span,
    ) -> Type {
        if operator == AssignmentOperator::Assign {
            self.require_assignable_value(&target, value, value_expression);
        } else if operator == AssignmentOperator::AddAssign
            && target == Type::String
            && *value == Type::String
        {
            // String concatenation is the sole non-numeric compound assignment.
        } else if let Some(kind) = bit_operation(operator) {
            self.bit_assignment(kind, &target, value, span);
        } else if let Some(operation_type) = promoted_numeric(&target, value) {
            if operation_type != target {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "compound assignment would narrow `{}` to `{}`",
                            operation_type.display(),
                            target.display()
                        ),
                        span,
                    )
                    .with_help(format!(
                        "write an explicit assignment and cast, for example `value = ({}) (value + other)`",
                        target.display()
                    )),
                );
            }
        } else {
            self.diagnostics.push(
                Diagnostic::error("compound assignment requires compatible operands", span)
                    .with_help("cast one operand explicitly to a safe common type"),
            );
        }
        target
    }

    /// Compound assignment with a bitwise or shift operator. The rules mirror
    /// the binary operators, and the operation type must equal the target type:
    /// ASTER never inserts a hidden narrowing conversion.
    fn bit_assignment(&mut self, kind: BitOperation, target: &Type, value: &Type, span: Span) {
        if *target == Type::Unknown || *value == Type::Unknown {
            return;
        }
        let operation_type = match kind {
            BitOperation::Logical if *target == Type::Bool && *value == Type::Bool => {
                Some(Type::Bool)
            }
            BitOperation::Logical if is_integer(target) && is_integer(value) => {
                promoted_numeric(target, value)
            }
            BitOperation::Shift if is_integer(target) && is_integer(value) => {
                if !is_shift_count(value) {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "shift count of type `{}` does not convert to `int`",
                                value.display()
                            ),
                            span,
                        )
                        .with_help("cast the count explicitly, e.g. `(int)count`"),
                    );
                    return;
                }
                target
                    .primitive()
                    .and_then(crate::primitives::promote_unary)
                    .map(Type::from_primitive)
            }
            _ => {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "compound assignment is not valid for `{}` and `{}`",
                            target.display(),
                            value.display()
                        ),
                        span,
                    )
                    .with_help("bitwise and shift assignment need integer operands, or `bool` for `&=`, `|=`, `^=`"),
                );
                return;
            }
        };
        match operation_type {
            Some(operation_type) if operation_type == *target => {}
            Some(operation_type) => self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "compound assignment would narrow `{}` to `{}`",
                        operation_type.display(),
                        target.display()
                    ),
                    span,
                )
                .with_help(format!(
                    "rewrite the operation as an explicit assignment and cast the result to `{}`",
                    target.display()
                )),
            ),
            None => {
                self.no_common_type(target, value, span);
            }
        }
    }

    /// Like `require_assignable`, but with the value expression available:
    /// a compile-time integer whose value fits the target type is accepted,
    /// so `byte b = 10;` works while `byte b = 300;` and `byte b = variable;`
    /// stay errors.
    pub(super) fn require_assignable_value(
        &mut self,
        expected: &Type,
        actual: &Type,
        value: &Expression,
    ) {
        if self.compatible(expected, actual)
            || *actual == Type::Unknown
            || *expected == Type::Unknown
        {
            return;
        }
        if let Some((constant, minimum, maximum)) =
            self.integer_constant_conversion(expected, actual, value)
        {
            if (minimum..=maximum).contains(&constant) {
                return;
            }
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "constant value `{constant}` does not fit `{}`",
                        expected.display()
                    ),
                    value.span,
                )
                .with_help(format!(
                    "`{}` holds {minimum} to {maximum}",
                    expected.display()
                )),
            );
            return;
        }
        self.require_assignable(expected, actual, value.span);
    }

    /// The integer-constant conversion rule behind `byte b = 10;` and constant
    /// call arguments: when an integer constant `value` (of integer type `actual`)
    /// is converted to the integer type `expected`, returns its value and the
    /// range of `expected`. The caller decides whether the value fits.
    pub(super) fn integer_constant_conversion(
        &self,
        expected: &Type,
        actual: &Type,
        value: &Expression,
    ) -> Option<(i128, i128, i128)> {
        let (target, source) = (expected.primitive()?, actual.primitive()?);
        if !target.is_integer() || !source.is_integer() {
            return None;
        }
        let (minimum, maximum) = target.integer_range()?;
        let resolve = |name: &str| self.binding(name).and_then(|binding| binding.value.clone());
        let constant = evaluate(value, &resolve)
            .ok()
            .as_ref()
            .and_then(integer_value)?;
        Some((constant, minimum, maximum))
    }

    pub(super) fn require_assignable(&mut self, expected: &Type, actual: &Type, span: Span) {
        if !self.compatible(expected, actual)
            && *actual != Type::Unknown
            && *expected != Type::Unknown
        {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "expected `{}`, found `{}`",
                        expected.display(),
                        actual.display()
                    ),
                    span,
                )
                .with_help(format!(
                    "use an expression of type `{}`",
                    expected.display()
                )),
            );
        }
    }

    pub(super) fn compatible(&self, expected: &Type, actual: &Type) -> bool {
        if compatible(expected, actual) {
            return true;
        }
        match (expected, actual) {
            // Identity-preserving upcast along the declared base chain.
            (Type::Class(base), Type::Class(derived)) => self.derives_from(derived, base),
            // `implemented_interfaces` includes the interfaces of the base
            // chain. In this step no member is virtual, so the table of the
            // static class is correct for every dynamic type.
            (Type::Interface(interface), Type::Class(class)) => {
                self.context.types.get(class).is_some_and(|info| {
                    info.implemented_interfaces
                        .iter()
                        .any(|implemented| implemented == interface)
                })
            }
            _ => false,
        }
    }

    pub(super) fn binding(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    pub(super) fn binding_mut(&mut self, name: &str) -> Option<&mut Binding> {
        self.scopes
            .iter_mut()
            .rev()
            .find_map(|scope| scope.get_mut(name))
    }
}

pub(super) fn needs_contextual_type(expression: &Expression) -> bool {
    match &expression.kind {
        ExpressionKind::ArrayLiteral(elements) => {
            elements.is_empty() || elements.iter().any(needs_contextual_type)
        }
        ExpressionKind::NewObject {
            type_name: None, ..
        } => true,
        ExpressionKind::ObjectInitializer { creation, .. } => needs_contextual_type(creation),
        ExpressionKind::Conditional {
            when_true,
            when_false,
            ..
        } => needs_contextual_type(when_true) || needs_contextual_type(when_false),
        ExpressionKind::Switch { cases, default, .. } => {
            cases.iter().any(|case| needs_contextual_type(&case.value))
                || default.as_deref().is_some_and(needs_contextual_type)
        }
        _ => false,
    }
}

/// Which family a bitwise or shift compound assignment belongs to.
#[derive(Clone, Copy)]
enum BitOperation {
    /// `&=`, `|=`, `^=`: integer or `bool` operands.
    Logical,
    /// `<<=`, `>>=`, `>>>=`: integer target and an `int`-convertible count.
    Shift,
}

fn bit_operation(operator: AssignmentOperator) -> Option<BitOperation> {
    match operator {
        AssignmentOperator::BitwiseAndAssign
        | AssignmentOperator::BitwiseOrAssign
        | AssignmentOperator::BitwiseXorAssign => Some(BitOperation::Logical),
        AssignmentOperator::ShiftLeftAssign
        | AssignmentOperator::ShiftRightAssign
        | AssignmentOperator::UnsignedShiftRightAssign => Some(BitOperation::Shift),
        _ => None,
    }
}

fn is_integer(type_: &Type) -> bool {
    type_.primitive().is_some_and(Primitive::is_integer)
}

/// A shift count must implicitly convert to `int`.
fn is_shift_count(type_: &Type) -> bool {
    type_
        .primitive()
        .is_some_and(|primitive| crate::primitives::implicit_converts(primitive, Primitive::Int))
}

/// The numeric type both operands are converted to, from the central promotion
/// table: the left operand alone decides for shifts (the count follows it), and
/// the common promoted type decides for every other numeric pair.
fn binary_operand_type(operator: BinaryOperator, left: &Type, right: &Type) -> Option<Type> {
    use BinaryOperator::{LogicalAnd, LogicalOr, ShiftLeft, ShiftRight, UnsignedShiftRight};
    match operator {
        LogicalAnd | LogicalOr => None,
        ShiftLeft | ShiftRight | UnsignedShiftRight => left
            .primitive()
            .and_then(crate::primitives::promote_unary)
            .map(Type::from_primitive),
        _ => promoted_numeric(left, right),
    }
}
