use super::{
    Analyzer, AssignmentOperator, BinaryOperator, Binding, Diagnostic, Expression, ExpressionKind,
    HashSet, IncrementOperator, IntegerFit, InterpolatedPart, Literal, Primitive, PropertyInfo,
    ResolvedPropagation, ResolvedPropertyAssignment, ResultCases, Span, Type, TypeKind, TypeName,
    TypeRef, UnaryOperator, UnsignedFit, Visibility, classify_integer, classify_unsigned,
    compatible, constant_integer, evaluate, fits_long, fits_ulong, integer_value, promoted_numeric,
    resolve_type_readonly, zero_initializable,
};

impl Analyzer<'_> {
    /// Whether `name`'s enum shares the nominal identity of the discovered
    /// official `aster.core.Result`. `false` when the core stdlib is absent.
    fn is_official_result(&self, name: &str) -> bool {
        self.context.official_result.as_deref()
            == TypeName::parse(name).map(|parsed| parsed.base).as_deref()
            && self.context.official_result.is_some()
    }

    fn is_official_option(&self, name: &str) -> bool {
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
        let Type::Enum(result_name) = operand_type else {
            if operand_type != Type::Unknown {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`?` requires an `aster.core.Result<T, E>` value, found `{}`",
                            operand_type.display()
                        ),
                        span,
                    )
                    .with_help("`?` propagates the `Error` case of an `aster.core.Result`"),
                );
            }
            return Type::Unknown;
        };
        if !self.is_official_result(&result_name) {
            if self.is_official_option(&result_name) {
                self.diagnostics.push(
                    Diagnostic::error(
                        "`?` does not support `aster.core.Option<T>` yet".to_owned(),
                        span,
                    )
                    .with_help("match the option with `switch`; `?` propagates `Result` only"),
                );
            } else {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`?` works only with `aster.core.Result<T, E>`, not `{result_name}`"
                        ),
                        span,
                    )
                    .with_help("only the official `aster.core.Result` supports `?`"),
                );
            }
            return Type::Unknown;
        }
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
            ResolvedPropagation {
                result_type: result_name,
                ok_index: operand_result.ok_index,
                error_index: operand_result.error_index,
                function_result_type: return_name,
                function_error_index: function_result.error_index,
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

    pub(super) fn expression(&mut self, expression: &Expression) -> Type {
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
            } => self.new_object(type_name, arguments, expression.span),
            ExpressionKind::Index { array, index } => self.index(array, index, expression.span),
            ExpressionKind::Name(name) => self.name(name, expression.span),
            ExpressionKind::This => self.this_expression(expression.span),
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
            } => self.conditional(condition, when_true, when_false),
            ExpressionKind::Cast { target, operand } => self.cast(target, operand, expression.span),
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                let left_type = self.expression(left);
                let right_type = self.expression(right);
                self.binary(*operator, &left_type, &right_type, expression.span)
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
            let actual = self.expression(&field.value);
            if !initialized.insert(field.name.as_str()) {
                self.diagnostics.push(Diagnostic::error(
                    format!("field `{}` is initialized more than once", field.name),
                    field.span,
                ));
                continue;
            }
            let Some(expected) = info.fields.get(&field.name) else {
                self.diagnostics.push(Diagnostic::error(
                    format!("struct `{type_name}` has no field `{}`", field.name),
                    field.span,
                ));
                continue;
            };
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
                        format!("missing field `{name}` in `{type_name}` literal"),
                        span,
                    )
                    .with_help("initialize every struct field explicitly"),
                );
            }
        }
        Type::User(type_name.to_owned())
    }

    fn this_expression(&mut self, span: Span) -> Type {
        if self.instance_context
            && let Some(owner) = self.owner.clone()
        {
            if self.constructor
                && self.field_names.iter().any(|field| {
                    self.binding(field)
                        .is_some_and(|binding| !binding.initialized)
                })
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "`this` cannot be used before all fields are initialized",
                        span,
                    )
                    .with_help("initialize every reference field before passing or using `this`"),
                );
            }
            Type::Class(owner.clone())
        } else {
            self.diagnostics.push(
                Diagnostic::error("`this` is valid only inside an instance method", span)
                    .with_help("access the object through an explicit variable"),
            );
            Type::Unknown
        }
    }

    fn array_literal(&mut self, elements: &[Expression], span: Span) -> Type {
        let Some(first) = elements.first() else {
            self.diagnostics.push(
                Diagnostic::error(
                    "cannot infer the element type of an empty array literal",
                    span,
                )
                .with_help("use `new T[0]` for an empty array"),
            );
            return Type::Unknown;
        };
        let element_type = self.expression(first);
        if matches!(element_type, Type::Array(_)) {
            self.diagnostics.push(
                Diagnostic::error("nested arrays are not implemented", span)
                    .with_help("use a one-dimensional array of scalar or struct values"),
            );
        }
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
        if constant_integer(length).is_some_and(|value| value < 0) {
            self.diagnostics.push(Diagnostic::error(
                "array length cannot be negative",
                length.span,
            ));
        }
        if matches!(element, Type::Void | Type::Unknown) {
            return Type::Unknown;
        }
        if !zero_initializable(&element, self.context, &mut HashSet::new()) {
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
                Diagnostic::error("array index must have type `int`", index.span)
                    .with_help("convert the index to `int`"),
            );
        }
        if constant_integer(index).is_some_and(|value| value < 0) {
            self.diagnostics.push(Diagnostic::error(
                "array index cannot be negative",
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
        let true_type = self.expression(when_true);
        let false_type = self.expression(when_false);
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

    fn binary(&mut self, operator: BinaryOperator, left: &Type, right: &Type, span: Span) -> Type {
        use BinaryOperator::{
            Add, Divide, Equal, Greater, GreaterEqual, Less, LessEqual, LogicalAnd, LogicalOr,
            Multiply, NotEqual, Remainder, Subtract,
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
            (Type::Class(left), Type::Class(right)) => left == right,
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

    #[allow(clippy::too_many_lines)]
    fn assignment(
        &mut self,
        target: &Expression,
        operator: AssignmentOperator,
        value: &Expression,
        span: Span,
    ) -> Type {
        let value_type = self.expression(value);
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
        if let ExpressionKind::Member { object, name } = &target.kind {
            let object_type = self.expression(object);
            if let Type::Class(type_name) = object_type
                && let Some(property) = self
                    .context
                    .types
                    .get(&type_name)
                    .and_then(|info| info.properties.get(name))
                    .cloned()
            {
                let Some(setter) = property.setter else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("property `{type_name}.{name}` is read-only"),
                            target.span,
                        )
                        .with_help("add a setter or assign to a mutable field inside the class"),
                    );
                    return Type::Unknown;
                };
                if setter.visibility == Visibility::Private
                    && self.owner.as_deref() != Some(type_name.as_str())
                {
                    self.diagnostics.push(Diagnostic::error(
                        format!("setter for property `{type_name}.{name}` is private"),
                        target.span,
                    ));
                }
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
                _ => {}
            }
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
        let ExpressionKind::Name(name) = &target.kind else {
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
        let Some(setter) = property.setter else {
            self.diagnostics.push(Diagnostic::error(
                format!("property `{owner}.{name}` is read-only"),
                target_span,
            ));
            return Type::Unknown;
        };
        if operator != AssignmentOperator::Assign && property.getter.is_none() {
            self.diagnostics.push(Diagnostic::error(
                format!("compound assignment requires getter for `{owner}.{name}`"),
                target_span,
            ));
            return Type::Unknown;
        }
        let result =
            self.assignment_types(operator, property.type_.clone(), value_type, value, span);
        self.model.property_assignments.insert(
            self.model_key(span),
            ResolvedPropertyAssignment {
                getter: property.getter.map(|getter| getter.key),
                setter: setter.key,
            },
        );
        result
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
        if let (Some(target), Some(source)) = (expected.primitive(), actual.primitive())
            && target.is_integer()
            && source.is_integer()
            && let Some((minimum, maximum)) = target.integer_range()
        {
            let evaluated = {
                let resolve =
                    |name: &str| self.binding(name).and_then(|binding| binding.value.clone());
                evaluate(value, &resolve)
                    .ok()
                    .as_ref()
                    .and_then(integer_value)
            };
            match evaluated {
                Some(constant) if (minimum..=maximum).contains(&constant) => return,
                Some(constant) => {
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
                None => {}
            }
        }
        self.require_assignable(expected, actual, value.span);
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
        let (Type::Interface(interface), Type::Class(class)) = (expected, actual) else {
            return false;
        };
        self.context.types.get(class).is_some_and(|info| {
            info.implemented_interfaces
                .iter()
                .any(|implemented| implemented == interface)
        })
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
