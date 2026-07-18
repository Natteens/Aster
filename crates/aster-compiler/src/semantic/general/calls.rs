use super::{
    Analyzer, Callable, Diagnostic, Dispatch, Expression, ExpressionKind, ResolvedCall,
    ResolvedEnumCase, Signature, Span, Type, TypeKind, Visibility,
};

impl Analyzer<'_> {
    pub(super) fn new_object(
        &mut self,
        type_name: &str,
        arguments: &[Expression],
        span: Span,
    ) -> Type {
        let Some(info) = self.context.types.get(type_name) else {
            self.diagnostics.push(Diagnostic::error(
                format!("unknown type `{type_name}`"),
                span,
            ));
            return Type::Unknown;
        };
        if info.is_static {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("static class `{type_name}` cannot be instantiated"),
                    span,
                )
                .with_help("call one of its static methods through the class name"),
            );
            for argument in arguments {
                self.expression(argument);
            }
            return Type::Unknown;
        }
        if info.kind != Some(TypeKind::Class) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("`new` requires a class, but `{type_name}` is not a class"),
                    span,
                )
                .with_help("construct structs with a named field literal"),
            );
            for argument in arguments {
                self.expression(argument);
            }
            return Type::Unknown;
        }
        let Some(signature) = info.constructor.clone() else {
            self.diagnostics.push(
                Diagnostic::error(format!("class `{type_name}` has no constructor"), span)
                    .with_help(format!("declare `{type_name}(...)` inside the class")),
            );
            for argument in arguments {
                self.expression(argument);
            }
            return Type::Class(type_name.to_owned());
        };
        if info.constructor_visibility == Some(Visibility::Private)
            && self.owner.as_deref() != Some(type_name)
        {
            self.diagnostics.push(Diagnostic::error(
                format!("constructor `{type_name}` is private"),
                span,
            ));
        }
        self.check_arguments(&signature.signature, arguments, span);
        self.model
            .constructors
            .insert(self.model_key(span), signature.key);
        Type::Class(type_name.to_owned())
    }

    pub(super) fn name(&mut self, name: &str, span: Span) -> Type {
        if let Some(binding) = self.binding(name).cloned() {
            if !binding.initialized {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("variable `{name}` is used before initialization"),
                        span,
                    )
                    .with_help("assign a value before reading the variable"),
                );
            }
            binding.type_
        } else if self.instance_context
            && let Some(owner) = &self.owner
            && let Some(property) = self
                .context
                .types
                .get(owner)
                .and_then(|info| info.properties.get(name))
        {
            let Some(getter) = &property.getter else {
                self.diagnostics.push(Diagnostic::error(
                    format!("property `{owner}.{name}` cannot be read"),
                    span,
                ));
                return Type::Unknown;
            };
            self.model
                .property_reads
                .insert(self.model_key(span), getter.key.clone());
            property.type_.clone()
        } else {
            self.diagnostics.push(
                Diagnostic::error(format!("unknown name `{name}`"), span)
                    .with_help("declare the name before using it"),
            );
            Type::Unknown
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn member(&mut self, object: &Expression, name: &str, span: Span) -> Type {
        if let ExpressionKind::Name(enum_name) = &object.kind
            && let Some(info) = self.context.types.get(enum_name)
            && info.kind == Some(TypeKind::Enum)
        {
            let Some((case_index, case)) = info
                .enum_cases
                .iter()
                .enumerate()
                .find(|(_, case)| case.name == name)
            else {
                self.diagnostics.push(Diagnostic::error(
                    format!("enum `{enum_name}` has no case `{name}`"),
                    span,
                ));
                return Type::Unknown;
            };
            if !case.fields.is_empty() {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("enum case `{enum_name}.{name}` requires payload arguments"),
                        span,
                    )
                    .with_help(format!("construct it as `{enum_name}.{name}(...)`")),
                );
                return Type::Unknown;
            }
            self.model.enum_values.insert(
                self.model_key(span),
                ResolvedEnumCase {
                    enum_name: enum_name.clone(),
                    case_index,
                },
            );
            return Type::Enum(enum_name.clone());
        }
        let object_type = self.expression(object);
        if matches!(object_type, Type::Array(_)) {
            if name == "Length" {
                return Type::Int;
            }
            self.diagnostics.push(Diagnostic::error(
                format!("array has no member `{name}`"),
                span,
            ));
            return Type::Unknown;
        }
        if object_type == Type::String {
            if name == "Length" {
                return Type::Int;
            }
            self.diagnostics.push(Diagnostic::error(
                format!("string has no member `{name}`"),
                span,
            ));
            return Type::Unknown;
        }
        let type_name = match object_type {
            Type::User(name) | Type::Class(name) | Type::Interface(name) => name,
            Type::Unknown => return Type::Unknown,
            other => {
                self.diagnostics.push(Diagnostic::error(
                    format!("type `{}` has no member `{name}`", other.display()),
                    span,
                ));
                return Type::Unknown;
            }
        };
        if self.constructor
            && self.owner.as_deref() == Some(type_name.as_str())
            && self.field_names.contains(name)
            && self
                .binding(name)
                .is_some_and(|binding| !binding.initialized)
        {
            self.diagnostics.push(Diagnostic::error(
                format!("field `{name}` is used before initialization"),
                span,
            ));
        }
        self.context.types.get(&type_name).map_or_else(
            || Type::Unknown,
            |info| {
                if let Some(property) = info.properties.get(name) {
                    let Some(getter) = &property.getter else {
                        self.diagnostics.push(Diagnostic::error(
                            format!("property `{type_name}.{name}` cannot be read"),
                            span,
                        ));
                        return Type::Unknown;
                    };
                    if getter.visibility == Visibility::Private
                        && self.owner.as_deref() != Some(type_name.as_str())
                    {
                        self.diagnostics.push(Diagnostic::error(
                            format!("getter for property `{type_name}.{name}` is private"),
                            span,
                        ));
                    }
                    self.model
                        .property_reads
                        .insert(self.model_key(span), getter.key.clone());
                    return property.type_.clone();
                }
                let Some(type_) = info.fields.get(name).cloned() else {
                    self.diagnostics.push(Diagnostic::error(
                        format!("type `{type_name}` has no field `{name}`"),
                        span,
                    ));
                    return Type::Unknown;
                };
                if info.field_visibility.get(name) == Some(&Visibility::Private)
                    && self.owner.as_deref() != Some(type_name.as_str())
                {
                    self.diagnostics.push(
                        Diagnostic::error(format!("field `{type_name}.{name}` is private"), span)
                            .with_help("access the value through a public field or method"),
                    );
                }
                type_
            },
        )
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn call(
        &mut self,
        callee: &Expression,
        arguments: &[Expression],
        span: Span,
    ) -> Type {
        if let ExpressionKind::Member { object, name } = &callee.kind
            && let ExpressionKind::Name(enum_name) = &object.kind
            && let Some(info) = self.context.types.get(enum_name)
            && info.kind == Some(TypeKind::Enum)
        {
            let enum_name = enum_name.clone();
            let cases = info.enum_cases.clone();
            let Some((case_index, case)) = cases
                .iter()
                .enumerate()
                .find(|(_, case)| case.name == *name)
            else {
                self.diagnostics.push(Diagnostic::error(
                    format!("enum `{enum_name}` has no case `{name}`"),
                    callee.span,
                ));
                for argument in arguments {
                    self.expression(argument);
                }
                return Type::Unknown;
            };
            let actual = arguments
                .iter()
                .map(|argument| self.expression(argument))
                .collect::<Vec<_>>();
            if actual.len() != case.fields.len() {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "enum case `{enum_name}.{name}` expects {} argument(s), found {}",
                        case.fields.len(),
                        actual.len()
                    ),
                    span,
                ));
            }
            for ((_, expected), (actual, expression)) in
                case.fields.iter().zip(actual.iter().zip(arguments))
            {
                self.require_assignable_value(expected, actual, expression);
            }
            self.model.enum_values.insert(
                self.model_key(span),
                ResolvedEnumCase {
                    enum_name: enum_name.clone(),
                    case_index,
                },
            );
            return Type::Enum(enum_name);
        }
        if let Some(level) = logging_level(callee) {
            return self.logging_call(level, arguments, span);
        }
        let calls_current_instance = matches!(&callee.kind, ExpressionKind::Name(name) if self.methods.contains_key(name))
            || matches!(&callee.kind, ExpressionKind::Member { object, .. } if matches!(object.kind, ExpressionKind::This));
        if self.constructor
            && calls_current_instance
            && self.field_names.iter().any(|field| {
                self.binding(field)
                    .is_some_and(|binding| !binding.initialized)
            })
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "cannot call an instance method before all fields are initialized",
                    span,
                )
                .with_help("initialize every reference field first"),
            );
        }
        let argument_types = arguments
            .iter()
            .map(|argument| self.expression(argument))
            .collect::<Vec<_>>();
        let resolved = match &callee.kind {
            ExpressionKind::Name(name) => {
                let candidates = self
                    .methods
                    .get(name)
                    .into_iter()
                    .flatten()
                    .filter(|candidate| candidate.is_static || self.instance_context)
                    .chain(self.context.functions.get(name).into_iter().flatten())
                    .cloned()
                    .collect::<Vec<_>>();
                self.resolve_overload(name, &candidates, &argument_types, span)
                    .map(|callable| {
                        let dispatch = if callable.is_static
                            || self.context.functions.get(name).is_some_and(|items| {
                                items.iter().any(|item| item.key == callable.key)
                            }) {
                            Dispatch::Direct
                        } else {
                            Dispatch::Instance
                        };
                        (callable, dispatch)
                    })
            }
            ExpressionKind::Member { object, name } => {
                if let ExpressionKind::Name(type_name) = &object.kind
                    && let Some(info) = self.context.types.get(type_name)
                {
                    let candidates = info
                        .methods
                        .get(name)
                        .into_iter()
                        .flatten()
                        .filter(|candidate| candidate.is_static)
                        .cloned()
                        .collect::<Vec<_>>();
                    if candidates.is_empty() {
                        self.diagnostics.push(Diagnostic::error(
                            format!("type `{type_name}` has no static method `{name}`"),
                            callee.span,
                        ));
                        None
                    } else {
                        self.resolve_overload(name, &candidates, &argument_types, span)
                            .map(|candidate| (candidate, Dispatch::Direct))
                    }
                } else {
                    let receiver = self.expression(object);
                    let interface_dispatch = matches!(receiver, Type::Interface(_));
                    let (Type::User(type_name)
                    | Type::Class(type_name)
                    | Type::Interface(type_name)) = receiver
                    else {
                        return Type::Unknown;
                    };
                    let candidates = self
                        .context
                        .types
                        .get(&type_name)
                        .and_then(|info| info.methods.get(name))
                        .into_iter()
                        .flatten()
                        .filter(|candidate| !candidate.is_static)
                        .cloned()
                        .collect::<Vec<_>>();
                    if candidates.is_empty() {
                        self.diagnostics.push(Diagnostic::error(
                            format!("type `{type_name}` has no method `{name}`"),
                            callee.span,
                        ));
                        return Type::Unknown;
                    }
                    let result = self.resolve_overload(name, &candidates, &argument_types, span);
                    if let Some(callable) = &result {
                        self.check_member_visibility(&type_name, name, callable.visibility, span);
                    }
                    result.map(|callable| {
                        let dispatch = if interface_dispatch {
                            Dispatch::Interface
                        } else {
                            Dispatch::Instance
                        };
                        (callable, dispatch)
                    })
                }
            }
            _ => None,
        };
        let Some((callable, dispatch)) = resolved else {
            if let ExpressionKind::Name(name) = &callee.kind
                && !self.instance_context
                && self
                    .context
                    .types
                    .values()
                    .any(|info| info.methods.contains_key(name))
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("instance method `{name}` requires an object"),
                        callee.span,
                    )
                    .with_help(format!("call it as `object.{name}(...)`")),
                );
                for argument in arguments {
                    self.expression(argument);
                }
                return Type::Unknown;
            }
            self.diagnostics.push(
                Diagnostic::error("expression is not a known callable", callee.span)
                    .with_help("call a declared namespace function or method"),
            );
            return Type::Unknown;
        };
        self.model.calls.insert(
            self.model_key(span),
            ResolvedCall {
                callable: callable.key,
                dispatch,
            },
        );
        callable.signature.result
    }

    fn resolve_overload(
        &mut self,
        name: &str,
        candidates: &[Callable],
        arguments: &[Type],
        span: Span,
    ) -> Option<Callable> {
        let mut ranked = candidates
            .iter()
            .filter(|candidate| candidate.signature.parameters.len() == arguments.len())
            .filter_map(|candidate| {
                candidate
                    .signature
                    .parameters
                    .iter()
                    .zip(arguments)
                    .try_fold(0u32, |score, (expected, actual)| {
                        if expected == actual {
                            Some(score)
                        } else if self.compatible(expected, actual) {
                            Some(score + 1)
                        } else {
                            None
                        }
                    })
                    .map(|score| (score, candidate))
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(score, candidate)| (*score, candidate.key.declaration_start));
        let Some((best_score, best)) = ranked.first() else {
            if let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.signature.parameters.len() == arguments.len())
            {
                for (expected, actual) in candidate.signature.parameters.iter().zip(arguments) {
                    if !self.compatible(expected, actual) {
                        self.diagnostics.push(Diagnostic::error(
                            format!(
                                "expected `{}`, found `{}`",
                                expected.display(),
                                actual.display()
                            ),
                            span,
                        ));
                    }
                }
                return None;
            }
            if let [candidate] = candidates {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "expected {} argument(s), found {}",
                        candidate.signature.parameters.len(),
                        arguments.len()
                    ),
                    span,
                ));
                return None;
            }
            self.diagnostics.push(
                Diagnostic::error(
                    format!("no overload of `{name}` accepts these argument types"),
                    span,
                )
                .with_help("use an exact signature or a documented safe implicit conversion"),
            );
            return None;
        };
        if ranked.get(1).is_some_and(|(score, _)| score == best_score) {
            self.diagnostics.push(
                Diagnostic::error(format!("call to `{name}` is ambiguous"), span)
                    .with_help("cast an argument explicitly to select one overload"),
            );
            return None;
        }
        Some((*best).clone())
    }

    fn check_member_visibility(
        &mut self,
        owner: &str,
        name: &str,
        visibility: Visibility,
        span: Span,
    ) {
        if visibility == Visibility::Private && self.owner.as_deref() != Some(owner) {
            self.diagnostics.push(Diagnostic::error(
                format!("method `{owner}.{name}` is private"),
                span,
            ));
        }
    }

    fn logging_call(&mut self, level: LogLevel<'_>, arguments: &[Expression], span: Span) -> Type {
        if matches!(level, LogLevel::Unknown("Info" | "Debug")) {
            self.diagnostics.push(
                Diagnostic::error(format!("`Log.{}` does not exist", level.name()), span)
                    .with_help("use `Log`, `Log.Warning`, or `Log.Error`"),
            );
        } else if matches!(level, LogLevel::Unknown(_)) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("unknown logging method `Log.{}`", level.name()),
                    span,
                )
                .with_help("use `Log`, `Log.Warning`, or `Log.Error`"),
            );
        }
        let signature = Signature {
            parameters: vec![Type::String],
            result: Type::Void,
        };
        self.check_arguments(&signature, arguments, span);
        Type::Void
    }

    fn check_arguments(&mut self, signature: &Signature, arguments: &[Expression], span: Span) {
        if arguments.len() != signature.parameters.len() {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "expected {} argument(s), found {}",
                        signature.parameters.len(),
                        arguments.len()
                    ),
                    span,
                )
                .with_help("pass exactly the parameters required by the callable"),
            );
        }
        for (argument, expected) in arguments.iter().zip(&signature.parameters) {
            let expected = expected.clone();
            let actual = self.expression(argument);
            self.require_assignable_value(&expected, &actual, argument);
        }
    }
}

fn logging_level(callee: &Expression) -> Option<LogLevel<'_>> {
    match &callee.kind {
        ExpressionKind::Name(name) if name == "Log" => Some(LogLevel::Normal),
        ExpressionKind::Member { object, name } => match &object.kind {
            ExpressionKind::Name(object) if object == "Log" => match name.as_str() {
                "Warning" => Some(LogLevel::Warning),
                "Error" => Some(LogLevel::Error),
                other => Some(LogLevel::Unknown(other)),
            },
            _ => None,
        },
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum LogLevel<'a> {
    Normal,
    Warning,
    Error,
    Unknown(&'a str),
}

impl LogLevel<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Normal => "Log",
            Self::Warning => "Warning",
            Self::Error => "Error",
            Self::Unknown(name) => name,
        }
    }
}
