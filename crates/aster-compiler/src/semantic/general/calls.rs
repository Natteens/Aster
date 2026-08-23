use super::{
    Analyzer, Argument, Callable, Diagnostic, Dispatch, Expression, ExpressionKind, HashSet, Model,
    Primitive, ResolvedArguments, ResolvedCall, ResolvedDefaultArgument,
    ResolvedDictionaryOperation, ResolvedEnumCase, ResolvedParallelFor, ResolvedParallelForEach,
    ResolvedParallelReduce, ResolvedStringBuilderOperation, ResolvedTaskRun, Signature, Span, Type,
    TypeKind, TypeRef, Visibility, resolve_type_readonly,
};
use aster_hir::StringOperation;

impl Analyzer<'_> {
    #[allow(clippy::too_many_lines)] // validates one constructor resolution transaction end to end
    pub(super) fn new_object(
        &mut self,
        type_name: &str,
        arguments: &[Argument],
        span: Span,
    ) -> Type {
        if type_name == "List" || type_name.starts_with("List<") {
            if !arguments.is_empty() {
                self.diagnostics.push(
                    Diagnostic::error("`new List<T>()` takes no arguments", span).with_help(
                        "remove the arguments; `List<T>` has no constructor parameters yet",
                    ),
                );
                for argument in arguments {
                    self.expression(argument);
                }
                return Type::Unknown;
            }
            // `List` is reserved (see `validate_no_reserved_type_names`) and
            // never registered in `context.types`, so it is recognized
            // structurally here instead of through the class lookup below.
            // Arity/`void`/`decimal` for the type argument itself were
            // already validated during monomorphization
            // (`Monomorphizer::concretize_type_name`); `resolve_type_readonly`
            // reproduces the same void/decimal/unknown collapse, so a
            // malformed element silently resolves to `Unknown` here without a
            // second, redundant diagnostic.
            return resolve_type_readonly(&TypeRef::new(type_name, span), self.context);
        }
        if type_name == "Dictionary" || type_name.starts_with("Dictionary<") {
            if !arguments.is_empty() {
                self.diagnostics.push(Diagnostic::error(
                    "`new Dictionary<K, V>()` takes no arguments",
                    span,
                ));
                for argument in arguments {
                    self.expression(argument);
                }
                return Type::Unknown;
            }
            let type_ = resolve_type_readonly(&TypeRef::new(type_name, span), self.context);
            if type_ == Type::Unknown {
                self.diagnostics.push(Diagnostic::error(format!("type `{type_name}` is not a valid concrete `Dictionary<K, V>` specialization"), span));
            }
            return type_;
        }
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
        let argument_types = arguments
            .iter()
            .map(|argument| {
                if super::expressions::needs_contextual_type(argument) {
                    Type::Unknown
                } else {
                    self.expression(argument)
                }
            })
            .collect::<Vec<_>>();
        if let Some((signature, plan)) = self.resolve_call_overload(
            type_name,
            std::slice::from_ref(&signature),
            &argument_types,
            arguments,
            span,
        ) {
            let key = self.model_key(span);
            self.model.constructors.insert(key.clone(), signature.key);
            self.model.call_arguments.insert(key, plan);
        }
        if type_name == crate::standard_library::STRING_BUILDER_NAME {
            self.model
                .string_builder_constructions
                .insert(self.model_key(span));
        }
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
        } else if !self.instance_context
            && let Some(owner) = self.owner.clone()
            && let Some(info) = self.context.types.get(&owner)
            && (info.fields.contains_key(name) || info.properties.contains_key(name))
        {
            let member_kind = if info.fields.contains_key(name) {
                "field"
            } else {
                "property"
            };
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "instance {member_kind} `{name}` cannot be used in a static context"
                    ),
                    span,
                )
                .with_help(format!(
                    "it requires an instance: create one with `new {owner}(...)` and access the member through it"
                )),
            );
            Type::Unknown
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
                    argument_order: Vec::new(),
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
        if matches!(object_type, Type::List(_)) {
            if matches!(name, "Length" | "Capacity") {
                return Type::Int;
            }
            self.diagnostics.push(Diagnostic::error(
                format!("list has no member `{name}`"),
                span,
            ));
            return Type::Unknown;
        }
        if matches!(object_type, Type::Dictionary(_, _)) {
            if matches!(name, "Length" | "Capacity") {
                return Type::Int;
            }
            self.diagnostics.push(Diagnostic::error(
                format!("dictionary has no member `{name}`"),
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
    pub(super) fn call(&mut self, callee: &Expression, arguments: &[Argument], span: Span) -> Type {
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
            let order = bind_enum_arguments(&case.fields, arguments);
            if order.is_none() {
                diagnose_enum_arguments(
                    &enum_name,
                    name,
                    &case.fields,
                    arguments,
                    span,
                    &mut self.diagnostics,
                );
            }
            let order = order.unwrap_or_else(|| (0..arguments.len()).collect());
            for (source, parameter) in order.iter().copied().enumerate() {
                let Some((_, expected)) = case.fields.get(parameter) else {
                    self.expression(&arguments[source]);
                    continue;
                };
                let actual = self.expression_expected(&arguments[source], Some(expected));
                self.require_assignable_value(expected, &actual, &arguments[source]);
            }
            self.model.enum_values.insert(
                self.model_key(span),
                ResolvedEnumCase {
                    enum_name: enum_name.clone(),
                    case_index,
                    argument_order: order,
                },
            );
            return Type::Enum(enum_name);
        }
        if let Some(level) = logging_level(callee) {
            return self.logging_call(level, arguments, span);
        }
        // `Task` is a reserved intrinsic name (no user declaration named
        // `Task` can exist; see `semantic::validate_no_reserved_type_names`),
        // so this structural shape check alone identifies `Task.Run`.
        if is_task_run_callee(callee) {
            return self.task_run(arguments, span);
        }
        if is_task_wait_all_callee(callee) {
            return self.task_wait_all(arguments, span);
        }
        if is_task_cancellation_requested_callee(callee) {
            if !arguments.is_empty() {
                self.diagnostics.push(Diagnostic::error(
                    "`Task.IsCancellationRequested` accepts no arguments",
                    span,
                ));
                for argument in arguments {
                    self.expression(argument);
                }
            }
            return Type::Bool;
        }
        if is_parallel_for_callee(callee) {
            return self.parallel_for(arguments, span);
        }
        if is_parallel_for_each_callee(callee) {
            return self.parallel_for_each(arguments, span);
        }
        if is_parallel_reduce_callee(callee) {
            return self.parallel_reduce(arguments, span);
        }
        if let ExpressionKind::Member { object, name } = &callee.kind
            && matches!(name.as_str(), "Wait" | "Cancel")
        {
            let object_type = self.expression(object);
            if let Type::Task(result_type) = object_type {
                if name == "Wait" && self.async_state != super::AsyncAnalysisState::OutsideAsync {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "`Wait()` is not supported inside an `async` function in this version",
                            span,
                        )
                        .with_help("only the function's own single `await` may suspend"),
                    );
                }
                if !arguments.is_empty() {
                    self.diagnostics.push(Diagnostic::error(
                        format!("`Task<T>.{name}` accepts no arguments"),
                        span,
                    ));
                    for argument in arguments {
                        self.expression(argument);
                    }
                }
                return if name == "Wait" {
                    *result_type
                } else {
                    Type::Bool
                };
            }
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
            .map(|argument| {
                if super::expressions::needs_contextual_type(argument) {
                    Type::Unknown
                } else {
                    self.expression(argument)
                }
            })
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
                // Every candidate was an instance method filtered out by the static
                // context: skip overload resolution so only the dedicated
                // "requires an object" diagnostic below is reported.
                if candidates.is_empty() && self.methods.contains_key(name) {
                    None
                } else {
                    let resolved = self.resolve_call_overload(
                        name,
                        &candidates,
                        &argument_types,
                        arguments,
                        span,
                    );
                    if !candidates.is_empty() && resolved.is_none() {
                        return Type::Unknown;
                    }
                    resolved.map(|(callable, plan)| {
                        let dispatch = if callable.is_static
                            || self.context.functions.get(name).is_some_and(|items| {
                                items.iter().any(|item| item.key == callable.key)
                            }) {
                            Dispatch::Direct
                        } else {
                            Dispatch::Instance
                        };
                        (callable, dispatch, plan)
                    })
                }
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
                        let resolved = self.resolve_call_overload(
                            name,
                            &candidates,
                            &argument_types,
                            arguments,
                            span,
                        );
                        let Some((candidate, plan)) = resolved else {
                            return Type::Unknown;
                        };
                        Some((candidate, Dispatch::Direct, plan))
                    }
                } else {
                    let receiver = self.expression(object);
                    if receiver == Type::String {
                        return self.string_operation(name, &argument_types, arguments, span);
                    }
                    if matches!(
                        receiver,
                        Type::Bool
                            | Type::Char
                            | Type::Int
                            | Type::UInt
                            | Type::Long
                            | Type::ULong
                            | Type::Float
                            | Type::Double
                    ) {
                        return self.format_primitive_operation(
                            &receiver,
                            name,
                            &argument_types,
                            span,
                        );
                    }
                    if let Type::List(element_type) = &receiver {
                        if name == "Add" {
                            if arguments.len() != 1 {
                                self.diagnostics.push(
                                    Diagnostic::error(
                                        format!(
                                            "`List<T>.Add` expects 1 argument, found {}",
                                            arguments.len()
                                        ),
                                        span,
                                    )
                                    .with_help("pass exactly one value of the list's element type"),
                                );
                                return Type::Unknown;
                            }
                            self.require_assignable_value(
                                element_type,
                                &argument_types[0],
                                &arguments[0],
                            );
                            return Type::Void;
                        }
                        if name == "Get" {
                            if arguments.len() != 1 {
                                self.diagnostics.push(
                                    Diagnostic::error(
                                        format!(
                                            "`List<T>.Get` expects 1 argument, found {}",
                                            arguments.len()
                                        ),
                                        span,
                                    )
                                    .with_help("pass a single `int` index"),
                                );
                                return Type::Unknown;
                            }
                            if argument_types[0] != Type::Int && argument_types[0] != Type::Unknown
                            {
                                self.diagnostics.push(
                                    Diagnostic::error(
                                        format!(
                                            "`List<T>.Get` requires an `int` index, found `{}`",
                                            argument_types[0].display()
                                        ),
                                        arguments[0].span,
                                    )
                                    .with_help("convert the index to `int`"),
                                );
                                return Type::Unknown;
                            }
                            return (**element_type).clone();
                        }
                        if name == "RemoveAt" {
                            if arguments.len() != 1 {
                                self.diagnostics.push(
                                    Diagnostic::error(
                                        format!(
                                            "`List<T>.RemoveAt` expects 1 argument, found {}",
                                            arguments.len()
                                        ),
                                        span,
                                    )
                                    .with_help("pass a single `int` index"),
                                );
                                return Type::Unknown;
                            }
                            if argument_types[0] != Type::Int && argument_types[0] != Type::Unknown
                            {
                                self.diagnostics.push(
                                    Diagnostic::error(
                                        format!(
                                            "`List<T>.RemoveAt` requires an `int` index, found `{}`",
                                            argument_types[0].display()
                                        ),
                                        arguments[0].span,
                                    )
                                    .with_help("convert the index to `int`"),
                                );
                                return Type::Unknown;
                            }
                            return Type::Void;
                        }
                        if name == "Set" {
                            if arguments.len() != 2 {
                                self.diagnostics.push(Diagnostic::error(
                                    format!(
                                        "`List<T>.Set` expects 2 arguments, found {}",
                                        arguments.len()
                                    ),
                                    span,
                                ));
                                return Type::Unknown;
                            }
                            if argument_types[0] != Type::Int && argument_types[0] != Type::Unknown
                            {
                                self.diagnostics.push(Diagnostic::error(
                                    format!(
                                        "`List<T>.Set` requires an `int` index, found `{}`",
                                        argument_types[0].display()
                                    ),
                                    arguments[0].span,
                                ));
                                return Type::Unknown;
                            }
                            self.require_assignable_value(
                                element_type,
                                &argument_types[1],
                                &arguments[1],
                            );
                            return Type::Void;
                        }
                        if name == "Clear" {
                            if !arguments.is_empty() {
                                self.diagnostics.push(Diagnostic::error(
                                    format!(
                                        "`List<T>.Clear` expects 0 arguments, found {}",
                                        arguments.len()
                                    ),
                                    span,
                                ));
                                return Type::Unknown;
                            }
                            return Type::Void;
                        }
                        if name == "ToArray" {
                            if !arguments.is_empty() {
                                self.diagnostics.push(Diagnostic::error(
                                    format!(
                                        "`List<T>.ToArray` expects 0 arguments, found {}",
                                        arguments.len()
                                    ),
                                    span,
                                ));
                                return Type::Unknown;
                            }
                            return Type::Array(element_type.clone());
                        }
                        if name == "EnsureCapacity" {
                            if arguments.len() != 1
                                || !matches!(
                                    argument_types.first(),
                                    Some(Type::Int | Type::Unknown)
                                )
                            {
                                self.diagnostics.push(Diagnostic::error(
                                    "`List<T>.EnsureCapacity` expects one `int` argument",
                                    span,
                                ));
                                return Type::Unknown;
                            }
                            return Type::Int;
                        }
                        if name == "AddRange" {
                            if arguments.len() != 1
                                || !matches!(argument_types.first(), Some(Type::Array(found)) if found.as_ref() == element_type.as_ref())
                            {
                                self.diagnostics.push(Diagnostic::error(
                                    format!(
                                        "`List<T>.AddRange` expects one `{}[]` argument",
                                        element_type.display()
                                    ),
                                    span,
                                ));
                                return Type::Unknown;
                            }
                            return Type::Void;
                        }
                        if name == "Insert" {
                            if arguments.len() != 2
                                || !matches!(
                                    argument_types.first(),
                                    Some(Type::Int | Type::Unknown)
                                )
                            {
                                self.diagnostics.push(Diagnostic::error(
                                    "`List<T>.Insert` expects an `int` index and one element",
                                    span,
                                ));
                                return Type::Unknown;
                            }
                            self.require_assignable_value(
                                element_type,
                                &argument_types[1],
                                &arguments[1],
                            );
                            return Type::Void;
                        }
                        if name == "RemoveRange" {
                            if arguments.len() != 2
                                || !argument_types
                                    .iter()
                                    .all(|type_| matches!(type_, Type::Int | Type::Unknown))
                            {
                                self.diagnostics.push(Diagnostic::error(
                                    "`List<T>.RemoveRange` expects `int` index and count arguments",
                                    span,
                                ));
                                return Type::Unknown;
                            }
                            return Type::Void;
                        }
                        if name == "Reverse" {
                            if !arguments.is_empty() {
                                self.diagnostics.push(Diagnostic::error(
                                    "`List<T>.Reverse` expects 0 arguments",
                                    span,
                                ));
                                return Type::Unknown;
                            }
                            return Type::Void;
                        }
                        if name == "GetRange" {
                            if arguments.len() != 2
                                || !argument_types
                                    .iter()
                                    .all(|type_| matches!(type_, Type::Int | Type::Unknown))
                            {
                                self.diagnostics.push(Diagnostic::error(
                                    "`List<T>.GetRange` expects `int` index and count arguments",
                                    span,
                                ));
                                return Type::Unknown;
                            }
                            return Type::Array(element_type.clone());
                        }
                        self.diagnostics.push(Diagnostic::error(
                            format!("list has no member `{name}`"),
                            callee.span,
                        ));
                        return Type::Unknown;
                    }
                    if let Type::Dictionary(key_type, value_type) = &receiver {
                        let expected_arity = match name.as_str() {
                            "Add" | "Set" | "GetOr" | "GetOrAdd" => Some(2),
                            "TryGet" | "ContainsKey" | "Remove" | "EnsureCapacity" => Some(1),
                            "Entries" | "Clear" | "Keys" | "Values" => Some(0),
                            _ => None,
                        };
                        let Some(expected_arity) = expected_arity else {
                            self.diagnostics.push(Diagnostic::error(
                                format!("dictionary has no member `{name}`"),
                                callee.span,
                            ));
                            return Type::Unknown;
                        };
                        if arguments.len() != expected_arity {
                            self.diagnostics.push(Diagnostic::error(
                                format!(
                                    "`Dictionary<K, V>.{name}` expects {expected_arity} argument(s), found {}",
                                    arguments.len()
                                ),
                                span,
                            ));
                            return Type::Unknown;
                        }
                        if let Some(argument) = arguments.first() {
                            if name == "EnsureCapacity" {
                                self.require_assignable_value(
                                    &Type::Int,
                                    &argument_types[0],
                                    argument,
                                );
                            } else {
                                self.require_assignable_value(
                                    key_type,
                                    &argument_types[0],
                                    argument,
                                );
                            }
                        }
                        if let Some(argument) = arguments.get(1) {
                            self.require_assignable_value(value_type, &argument_types[1], argument);
                        }
                        match name.as_str() {
                            "EnsureCapacity" => {
                                self.model.dictionary_operations.insert(
                                    self.model_key(span),
                                    ResolvedDictionaryOperation::Basic,
                                );
                                return Type::Int;
                            }
                            "GetOr" | "GetOrAdd" => {
                                self.model.dictionary_operations.insert(
                                    self.model_key(span),
                                    ResolvedDictionaryOperation::Basic,
                                );
                                return (**value_type).clone();
                            }
                            "Add" | "Set" | "ContainsKey" | "Remove" => {
                                self.model.dictionary_operations.insert(
                                    self.model_key(span),
                                    ResolvedDictionaryOperation::Basic,
                                );
                                return Type::Bool;
                            }
                            "Clear" => {
                                self.model.dictionary_operations.insert(
                                    self.model_key(span),
                                    ResolvedDictionaryOperation::Basic,
                                );
                                return Type::Void;
                            }
                            "Keys" => {
                                self.model.dictionary_operations.insert(
                                    self.model_key(span),
                                    ResolvedDictionaryOperation::Basic,
                                );
                                return Type::Array(key_type.clone());
                            }
                            "Values" => {
                                self.model.dictionary_operations.insert(
                                    self.model_key(span),
                                    ResolvedDictionaryOperation::Basic,
                                );
                                return Type::Array(value_type.clone());
                            }
                            "TryGet" => {
                                let option_name =
                                    crate::standard_library::option_specialization_name(
                                        &value_type.display(),
                                    );
                                let Some(option) = self.option_cases(&option_name) else {
                                    self.diagnostics.push(
                                        Diagnostic::error(
                                            format!(
                                                "`Dictionary<K, V>.TryGet` requires the official `{option_name}` type"
                                            ),
                                            span,
                                        )
                                        .with_help("add `using aster.core;`"),
                                    );
                                    return Type::Unknown;
                                };
                                if !self.is_official_option(&option_name) {
                                    self.diagnostics.push(Diagnostic::error(
                                        "`Dictionary<K, V>.TryGet` requires the official `aster.core.Option<V>`",
                                        span,
                                    ));
                                    return Type::Unknown;
                                }
                                self.model.dictionary_operations.insert(
                                    self.model_key(span),
                                    ResolvedDictionaryOperation::TryGet {
                                        option_type: option_name.clone(),
                                        some_index: option.some_index,
                                        none_index: option.none_index,
                                    },
                                );
                                return Type::Enum(option_name);
                            }
                            "Entries" => {
                                let entry_name =
                                    crate::standard_library::dictionary_entry_specialization_name(
                                        &key_type.display(),
                                        &value_type.display(),
                                    );
                                if !self.context.types.contains_key(&entry_name) {
                                    self.diagnostics.push(
                                        Diagnostic::error(
                                            format!(
                                                "`Dictionary<K, V>.Entries` requires the official `{entry_name}` type"
                                            ),
                                            span,
                                        )
                                        .with_help("add `using aster.collections;`"),
                                    );
                                    return Type::Unknown;
                                }
                                self.model.dictionary_operations.insert(
                                    self.model_key(span),
                                    ResolvedDictionaryOperation::Entries {
                                        entry_type: entry_name.clone(),
                                    },
                                );
                                return Type::Array(Box::new(Type::User(entry_name)));
                            }
                            _ => unreachable!("matched Dictionary method above"),
                        }
                    }
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
                    let result = self.resolve_call_overload(
                        name,
                        &candidates,
                        &argument_types,
                        arguments,
                        span,
                    );
                    if let Some((callable, _)) = &result {
                        self.check_member_visibility(&type_name, name, callable.visibility, span);
                    }
                    if result.is_none() {
                        return Type::Unknown;
                    }
                    result.map(|(callable, plan)| {
                        let dispatch = if interface_dispatch {
                            Dispatch::Interface
                        } else {
                            Dispatch::Instance
                        };
                        (callable, dispatch, plan)
                    })
                }
            }
            _ => None,
        };
        let Some((callable, dispatch, argument_plan)) = resolved else {
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
        let model_key = self.model_key(span);
        let callable_key = callable.key.clone();
        if callable.is_foreign {
            if self.unsafe_depth == 0 {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "foreign call `{}` requires an `unsafe` block",
                            callable_key.name
                        ),
                        span,
                    )
                    .with_help("wrap only the foreign call in `unsafe { ... }`"),
                );
            } else {
                self.model.foreign_calls.insert(model_key.clone());
            }
        }
        if callable_key.owner.as_deref() == Some(crate::standard_library::STRING_BUILDER_NAME) {
            let operation = match callable_key.name.as_str() {
                "Append"
                    if matches!(
                        callable.signature.parameters.as_slice(),
                        [Type::String
                            | Type::Bool
                            | Type::Char
                            | Type::SByte
                            | Type::Byte
                            | Type::Short
                            | Type::UShort
                            | Type::Int
                            | Type::UInt
                            | Type::Long
                            | Type::ULong
                            | Type::Float
                            | Type::Double]
                    ) =>
                {
                    Some(ResolvedStringBuilderOperation::Append)
                }
                "ToString" => Some(ResolvedStringBuilderOperation::ToString),
                "Clear" => Some(ResolvedStringBuilderOperation::Clear),
                _ => None,
            };
            if let Some(operation) = operation {
                self.model
                    .string_builder_operations
                    .insert(model_key.clone(), operation);
            }
        }
        self.model.calls.insert(
            model_key.clone(),
            ResolvedCall {
                callable: callable_key,
                dispatch,
            },
        );
        self.model.call_arguments.insert(model_key, argument_plan);
        callable.signature.result
    }

    /// Resolves `value.ToString()` on one of the eight fundamental
    /// primitives (`bool`/`char`/`int`/`uint`/`long`/`ulong`/`float`/
    /// `double`). No other member is exposed on these receivers here, so a
    /// user type can never collide with this intrinsic: only the resolver's
    /// primitive-type branch reaches this function at all.
    fn format_primitive_operation(
        &mut self,
        receiver: &Type,
        name: &str,
        argument_types: &[Type],
        span: Span,
    ) -> Type {
        if name != "ToString" {
            self.diagnostics.push(Diagnostic::error(
                format!("`{}` has no method `{name}`", receiver.display()),
                span,
            ));
            return Type::Unknown;
        }
        if !argument_types.is_empty() {
            self.diagnostics.push(Diagnostic::error(
                format!(
                    "`{}.ToString` expects 0 arguments, found {}",
                    receiver.display(),
                    argument_types.len()
                ),
                span,
            ));
            return Type::Unknown;
        }
        self.model.format_primitives.insert(self.model_key(span));
        Type::String
    }

    fn string_operation(
        &mut self,
        name: &str,
        argument_types: &[Type],
        arguments: &[Argument],
        span: Span,
    ) -> Type {
        let (operation, parameters, result) = match (name, argument_types.len()) {
            ("Contains", 1) => (StringOperation::Contains, vec![Type::String], Type::Bool),
            ("StartsWith", 1) => (StringOperation::StartsWith, vec![Type::String], Type::Bool),
            ("EndsWith", 1) => (StringOperation::EndsWith, vec![Type::String], Type::Bool),
            ("IndexOf", 1) => (StringOperation::IndexOf, vec![Type::String], Type::Int),
            ("Substring", 1) => (
                StringOperation::SubstringFrom,
                vec![Type::Int],
                Type::String,
            ),
            ("Substring", 2) => (
                StringOperation::SubstringRange,
                vec![Type::Int, Type::Int],
                Type::String,
            ),
            ("Contains" | "StartsWith" | "EndsWith" | "IndexOf", found) => {
                self.diagnostics.push(Diagnostic::error(
                    format!("`string.{name}` expects 1 argument, found {found}"),
                    span,
                ));
                return Type::Unknown;
            }
            ("Substring", found) => {
                self.diagnostics.push(Diagnostic::error(
                    format!("`string.Substring` expects 1 or 2 arguments, found {found}"),
                    span,
                ));
                return Type::Unknown;
            }
            (
                "TryParseBool" | "TryParseChar" | "TryParseSByte" | "TryParseByte"
                | "TryParseShort" | "TryParseUShort" | "TryParseInt" | "TryParseUInt"
                | "TryParseLong" | "TryParseULong" | "TryParseFloat" | "TryParseDouble",
                0,
            ) => match self.try_parse_operation(name, span) {
                Some(result) => result,
                None => return Type::Unknown,
            },
            (
                "TryParseBool" | "TryParseChar" | "TryParseSByte" | "TryParseByte"
                | "TryParseShort" | "TryParseUShort" | "TryParseInt" | "TryParseUInt"
                | "TryParseLong" | "TryParseULong" | "TryParseFloat" | "TryParseDouble",
                found,
            ) => {
                self.diagnostics.push(Diagnostic::error(
                    format!("`string.{name}` expects 0 arguments, found {found}"),
                    span,
                ));
                return Type::Unknown;
            }
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    format!("string has no method `{name}`"),
                    span,
                ));
                return Type::Unknown;
            }
        };

        let mut incompatible = false;
        for ((actual, expected), argument) in argument_types.iter().zip(&parameters).zip(arguments)
        {
            if actual != expected && *actual != Type::Unknown {
                incompatible = true;
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "expected `{}`, found `{}`",
                        expected.display(),
                        actual.display()
                    ),
                    argument.span,
                ));
            }
        }
        if incompatible {
            return Type::Unknown;
        }
        self.model
            .string_operations
            .insert(self.model_key(span), operation);
        result
    }

    /// Resolves one zero-argument `string.TryParse*()` call to its
    /// `StringOperation` and `Option<target>` result type. `None` means a
    /// diagnostic was already pushed and the caller should return
    /// `Type::Unknown`.
    fn try_parse_operation(
        &mut self,
        name: &str,
        span: Span,
    ) -> Option<(StringOperation, Vec<Type>, Type)> {
        let operation = match name {
            "TryParseBool" => StringOperation::TryParseBool,
            "TryParseChar" => StringOperation::TryParseChar,
            "TryParseSByte" => StringOperation::TryParseSByte,
            "TryParseByte" => StringOperation::TryParseByte,
            "TryParseShort" => StringOperation::TryParseShort,
            "TryParseUShort" => StringOperation::TryParseUShort,
            "TryParseInt" => StringOperation::TryParseInt,
            "TryParseUInt" => StringOperation::TryParseUInt,
            "TryParseLong" => StringOperation::TryParseLong,
            "TryParseULong" => StringOperation::TryParseULong,
            "TryParseFloat" => StringOperation::TryParseFloat,
            _ => StringOperation::TryParseDouble,
        };
        let target = operation
            .parse_target_name()
            .expect("TryParse* always names a parse target");
        // `official_option` being absent means `aster.core` was never linked
        // in at all; reuse the same discovered nominal identity `?` already
        // trusts, rather than guessing a type spelling of its own.
        if self.context.official_option.is_none() {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("`string.{name}` requires `aster.core.Option<{target}>`"),
                    span,
                )
                .with_help("add `using aster.core;` to this file"),
            );
            return None;
        }
        let option_name = crate::standard_library::option_specialization_name(target);
        if !self.context.types.contains_key(&option_name) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`string.{name}` requires the `Option<{target}>` specialization, which was not generated"
                    ),
                    span,
                )
                .with_help("this indicates an internal compiler error, not a source mistake"),
            );
            return None;
        }
        Some((operation, Vec::new(), Type::Enum(option_name)))
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

    #[allow(clippy::too_many_lines)] // ranking, contextual validation, and default planning stay atomic
    fn resolve_call_overload(
        &mut self,
        name: &str,
        candidates: &[Callable],
        argument_types: &[Type],
        arguments: &[Argument],
        span: Span,
    ) -> Option<(Callable, ResolvedArguments)> {
        let mut ranked = candidates
            .iter()
            .filter_map(|candidate| {
                let mapping = bind_arguments(candidate, arguments)?;
                let score =
                    mapping
                        .iter()
                        .enumerate()
                        .try_fold(0u32, |score, (source, parameter)| {
                            let expected = &candidate.signature.parameters[*parameter];
                            let actual = &argument_types[source];
                            if super::expressions::needs_contextual_type(&arguments[source]) {
                                self.contextual_argument_compatible(&arguments[source], expected)
                                    .then_some(score)
                            } else if expected == actual {
                                Some(score)
                            } else if *actual == Type::Unknown {
                                Some(score + 2)
                            } else if self.compatible(expected, actual) {
                                Some(score + 1)
                            } else {
                                None
                            }
                        })?;
                Some((score, candidate, mapping))
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|(score, candidate, _)| (*score, candidate.key.declaration_start));
        let Some((best_score, candidate, mapping)) = ranked.first().cloned() else {
            if let [candidate] = candidates {
                self.diagnose_argument_binding(candidate, arguments, span);
                if let Some(mapping) = bind_arguments(candidate, arguments) {
                    for (source, parameter) in mapping.into_iter().enumerate() {
                        let expected = &candidate.signature.parameters[parameter];
                        let actual =
                            if super::expressions::needs_contextual_type(&arguments[source]) {
                                self.expression_expected(&arguments[source], Some(expected))
                            } else {
                                argument_types[source].clone()
                            };
                        self.require_assignable_value(expected, &actual, &arguments[source]);
                    }
                }
            } else {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("no overload of `{name}` accepts these arguments"),
                        span,
                    )
                    .with_help("check argument names, required parameters, and exact types"),
                );
            }
            return None;
        };
        if ranked
            .get(1)
            .is_some_and(|(score, _, _)| *score == best_score)
        {
            self.diagnostics.push(
                Diagnostic::error(format!("call to `{name}` is ambiguous"), span)
                    .with_help("use explicit argument types or names that select one overload"),
            );
            return None;
        }

        for (source, parameter) in mapping.iter().copied().enumerate() {
            let expected = &candidate.signature.parameters[parameter];
            let actual = if super::expressions::needs_contextual_type(&arguments[source]) {
                self.expression_expected(&arguments[source], Some(expected))
            } else {
                argument_types[source].clone()
            };
            self.require_assignable_value(expected, &actual, &arguments[source]);
        }

        let mut defaults = Vec::new();
        let supplied = mapping.iter().copied().collect::<HashSet<_>>();
        for (parameter, default) in candidate.defaults.iter().enumerate() {
            if supplied.contains(&parameter) {
                continue;
            }
            let Some(default) = default else {
                continue;
            };
            // Defaults belong to the declaration, not to the caller's local
            // scope. A caller-local constant with the same spelling must not
            // capture or alter an omitted argument.
            let resolve = |name: &str| {
                self.context
                    .globals
                    .get(name)
                    .and_then(|binding| binding.value.clone())
            };
            if let Ok(value) = super::evaluate(default, &resolve) {
                defaults.push(ResolvedDefaultArgument {
                    parameter,
                    value: value.coerce_to(&candidate.signature.parameters[parameter].display()),
                });
            }
        }
        Some((
            candidate.clone(),
            ResolvedArguments {
                source_to_parameter: mapping,
                defaults,
            },
        ))
    }

    /// Candidate-specific contextual checking must consider the complete
    /// expression, including already-typed siblings beside `[]`/`new()`.
    /// Analyze against a scratch model so overload probing cannot initialize
    /// bindings, emit diagnostics, or leave a losing candidate's resolutions
    /// in the real semantic model.
    fn contextual_argument_compatible(&self, argument: &Expression, expected: &Type) -> bool {
        let mut model = Model::default();
        let mut analyzer = Analyzer {
            context: self.context,
            scopes: self.scopes.clone(),
            return_type: self.return_type.clone(),
            methods: self.methods.clone(),
            owner: self.owner.clone(),
            constructor: self.constructor,
            instance_context: self.instance_context,
            model: &mut model,
            field_names: self.field_names.clone(),
            diagnostics: Vec::new(),
            loop_depth: self.loop_depth,
            model_context: self.model_context.clone(),
            async_state: self.async_state,
            unsafe_depth: self.unsafe_depth,
        };
        let actual = analyzer.expression_expected(argument, Some(expected));
        analyzer.require_assignable_value(expected, &actual, argument);
        analyzer.diagnostics.is_empty()
    }

    fn diagnose_argument_binding(
        &mut self,
        candidate: &Callable,
        arguments: &[Argument],
        span: Span,
    ) {
        let mut occupied = vec![false; candidate.signature.parameters.len()];
        let mut next = 0usize;
        let mut saw_named = false;
        for argument in arguments {
            let parameter = if let Some(name) = &argument.name {
                saw_named = true;
                let Some(parameter) = candidate
                    .parameter_names
                    .iter()
                    .position(|parameter| parameter == name)
                else {
                    self.diagnostics.push(Diagnostic::error(
                        format!("unknown named argument `{name}`"),
                        argument.span,
                    ));
                    continue;
                };
                parameter
            } else {
                if saw_named {
                    self.diagnostics.push(Diagnostic::error(
                        "positional arguments must precede named arguments",
                        argument.span,
                    ));
                }
                while next < occupied.len() && occupied[next] {
                    next += 1;
                }
                if next == occupied.len() {
                    self.diagnostics.push(Diagnostic::error(
                        "too many arguments for this callable",
                        argument.span,
                    ));
                    continue;
                }
                let parameter = next;
                next += 1;
                parameter
            };
            if std::mem::replace(&mut occupied[parameter], true) {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "parameter `{}` is supplied more than once",
                        candidate.parameter_names[parameter]
                    ),
                    argument.span,
                ));
            }
        }
        for (parameter, supplied) in occupied.into_iter().enumerate() {
            if !supplied && candidate.defaults[parameter].is_none() {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "missing required argument `{}`",
                        candidate.parameter_names[parameter]
                    ),
                    span,
                ));
            }
        }
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

    fn logging_call(&mut self, level: LogLevel<'_>, arguments: &[Argument], span: Span) -> Type {
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

    /// `aster.core.Task.Run(function, arguments...)`. The callable is
    /// resolved from the concrete argument types, while every runtime
    /// argument remains an ordinary caller-evaluated expression.
    fn task_run(&mut self, arguments: &[Argument], span: Span) -> Type {
        let Some((target, values)) = arguments.split_first() else {
            self.diagnostics.push(Diagnostic::error(
                "`Task.Run` expects a function argument",
                span,
            ));
            return Type::Unknown;
        };
        let value_types = values
            .iter()
            .map(|argument| self.expression(argument))
            .collect::<Vec<_>>();
        let Some(candidate) = self.resolve_task_run_target(target, &value_types) else {
            return Type::Unknown;
        };
        if !transferable(&candidate.signature.result) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`Task<{}>` cannot cross a worker boundary in this version",
                        candidate.signature.result.display()
                    ),
                    target.span,
                )
                .with_help(
                    "Task.Run currently supports only scalar results (bool, char, integers, floats)",
                ),
            );
            return Type::Unknown;
        }
        for (value, type_) in values.iter().zip(&candidate.signature.parameters) {
            if !self.worker_transferable_layout(type_, &mut HashSet::new()) {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`{}` cannot cross a worker boundary as a `Task.Run` argument",
                            type_.display()
                        ),
                        value.span,
                    )
                    .with_help(
                        "use a scalar or a finite struct/enum containing only transferable values",
                    ),
                );
            }
        }
        let result_type = candidate.signature.result.clone();
        self.model.task_runs.insert(
            self.model_key(span),
            ResolvedTaskRun {
                function: candidate.key,
            },
        );
        Type::Task(Box::new(result_type))
    }

    /// Resolve `Task.Run`'s target against the concrete runtime argument
    /// types. The target must directly name a free function or static method;
    /// variables, instance methods, and other expression shapes never become
    /// captured or bound callables.
    fn resolve_task_run_target(
        &mut self,
        argument: &Expression,
        argument_types: &[Type],
    ) -> Option<Callable> {
        match &argument.kind {
            ExpressionKind::Name(name) => {
                let mut candidates = self
                    .context
                    .functions
                    .get(name)
                    .into_iter()
                    .flatten()
                    .filter(|candidate| candidate.is_static)
                    .cloned()
                    .collect::<Vec<_>>();
                candidates.extend(
                    self.methods
                        .get(name)
                        .into_iter()
                        .flatten()
                        .filter(|candidate| candidate.is_static)
                        .cloned(),
                );
                self.resolve_overload(name, &candidates, argument_types, argument.span)
            }
            ExpressionKind::Member { object, name } => {
                let ExpressionKind::Name(type_name) = &object.kind else {
                    self.diagnostics.push(Diagnostic::error(
                        "`Task.Run` argument must directly name a static method or free function",
                        argument.span,
                    ));
                    return None;
                };
                let Some(info) = self.context.types.get(type_name) else {
                    self.diagnostics.push(Diagnostic::error(
                        format!("unknown type `{type_name}`"),
                        argument.span,
                    ));
                    return None;
                };
                let candidates = info
                    .methods
                    .get(name)
                    .into_iter()
                    .flatten()
                    .filter(|candidate| candidate.is_static)
                    .cloned()
                    .collect::<Vec<_>>();
                self.resolve_overload(name, &candidates, argument_types, argument.span)
            }
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    "`Task.Run` argument must directly name a static method or free function",
                    argument.span,
                ));
                None
            }
        }
    }

    fn task_wait_all(&mut self, arguments: &[Argument], span: Span) -> Type {
        let [tasks] = arguments else {
            self.diagnostics.push(Diagnostic::error(
                format!(
                    "`Task.WaitAll` expects exactly one task-array argument, found {}",
                    arguments.len()
                ),
                span,
            ));
            for argument in arguments {
                self.expression(argument);
            }
            return Type::Unknown;
        };
        let type_ = self.expression(tasks);
        let Type::Array(element) = type_ else {
            self.diagnostics.push(Diagnostic::error(
                "`Task.WaitAll` requires a homogeneous `Task<T>[]` array",
                tasks.span,
            ));
            return Type::Unknown;
        };
        let Type::Task(result) = *element else {
            self.diagnostics.push(Diagnostic::error(
                "`Task.WaitAll` requires a homogeneous `Task<T>[]` array",
                tasks.span,
            ));
            return Type::Unknown;
        };
        if !transferable(&result) {
            self.diagnostics.push(Diagnostic::error(
                format!(
                    "`Task.WaitAll` cannot compose `Task<{}>` results",
                    result.display()
                ),
                tasks.span,
            ));
            return Type::Unknown;
        }
        Type::Array(result)
    }

    /// One semantic worker-transfer authority. Primitive scalars defer to
    /// `aster-types`; finite structs/enums recurse nominally through their
    /// already-specialized field layouts. Reference-bearing and recursive
    /// shapes fail closed.
    fn worker_transferable_layout(&self, type_: &Type, visiting: &mut HashSet<String>) -> bool {
        if transferable(type_) {
            return true;
        }
        let (name, is_enum) = match type_ {
            Type::User(name) => (name, false),
            Type::Enum(name) => (name, true),
            _ => return false,
        };
        if !visiting.insert(name.clone()) {
            return false;
        }
        let Some(info) = self.context.types.get(name) else {
            visiting.remove(name);
            return false;
        };
        let result = if is_enum {
            info.enum_cases.iter().all(|case| {
                case.fields
                    .iter()
                    .all(|(_, field)| self.worker_transferable_layout(field, visiting))
            })
        } else {
            info.fields
                .values()
                .all(|field| self.worker_transferable_layout(field, visiting))
        };
        visiting.remove(name);
        result
    }

    /// `Parallel.For(start, end, Body)`: `start`/`end` must be `int`,
    /// evaluated left to right exactly once by the caller (`Analyzer::call`'s
    /// normal argument evaluation happens inside `resolve_parallel_body_target`
    /// and here for `start`/`end`); `Body` must directly name a resolvable
    /// zero-capture free function or static method `void(int)`.
    fn parallel_for(&mut self, arguments: &[Argument], span: Span) -> Type {
        let [start, end, body] = arguments else {
            self.diagnostics.push(Diagnostic::error(
                format!(
                    "`Parallel.For` expects exactly 3 arguments, found {}",
                    arguments.len()
                ),
                span,
            ));
            for argument in arguments {
                self.expression(argument);
            }
            return Type::Void;
        };
        let start_type = self.expression(start);
        let end_type = self.expression(end);
        self.require_assignable_value(&Type::Int, &start_type, start);
        self.require_assignable_value(&Type::Int, &end_type, end);
        let Some(candidate) = self.resolve_parallel_body_target(body, &[Type::Int]) else {
            return Type::Void;
        };
        self.model.parallel_for.insert(
            self.model_key(span),
            ResolvedParallelFor {
                body: candidate.key,
            },
        );
        Type::Void
    }

    /// `Parallel.ForEach(values, Body)`: `values` must be an array of a
    /// scalar transferable element type; `Body` must directly name a
    /// resolvable zero-capture free function or static method `void(T)`.
    fn parallel_for_each(&mut self, arguments: &[Argument], span: Span) -> Type {
        let [values, body] = arguments else {
            self.diagnostics.push(Diagnostic::error(
                format!(
                    "`Parallel.ForEach` expects exactly 2 arguments, found {}",
                    arguments.len()
                ),
                span,
            ));
            for argument in arguments {
                self.expression(argument);
            }
            return Type::Void;
        };
        let values_type = self.expression(values);
        let Type::Array(element_type) = values_type else {
            self.diagnostics.push(Diagnostic::error(
                "`Parallel.ForEach` requires an array argument",
                values.span,
            ));
            self.expression(body);
            return Type::Void;
        };
        if !transferable(&element_type) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`Parallel.ForEach` over `{}[]` requires a scalar element type in this version",
                        element_type.display()
                    ),
                    values.span,
                )
                .with_help("Parallel.ForEach currently supports only scalar elements: bool, char, integers, floats"),
            );
            self.expression(body);
            return Type::Void;
        }
        let Some(candidate) = self.resolve_parallel_body_target(body, &[(*element_type).clone()])
        else {
            return Type::Void;
        };
        self.model.parallel_for_each.insert(
            self.model_key(span),
            ResolvedParallelForEach {
                body: candidate.key,
            },
        );
        Type::Void
    }

    /// `Parallel.Reduce(values, identity, Accumulate, Combine)`: `values` must
    /// be an array of a worker-transferable element type (`TElement`);
    /// `identity` must itself be worker-transferable, and its type fixes the
    /// accumulator type (`TAccumulator`) for this call. `Accumulate` must
    /// directly name a resolvable zero-capture free function or static
    /// method `TAccumulator(TAccumulator, TElement)`; `Combine` must directly
    /// name one `TAccumulator(TAccumulator, TAccumulator)`. Arguments are
    /// evaluated left to right, exactly once: `values`, then `identity`;
    /// `Accumulate`/`Combine` are never evaluated as runtime expressions,
    /// only resolved structurally, exactly like `Parallel.For`/`ForEach`'s body.
    fn parallel_reduce(&mut self, arguments: &[Argument], span: Span) -> Type {
        let [values, identity, accumulate_argument, combine_argument] = arguments else {
            self.diagnostics.push(Diagnostic::error(
                format!(
                    "`Parallel.Reduce` expects exactly 4 arguments, found {}",
                    arguments.len()
                ),
                span,
            ));
            for argument in arguments {
                self.expression(argument);
            }
            return Type::Unknown;
        };
        let values_type = self.expression(values);
        let Type::Array(element_type) = values_type else {
            self.diagnostics.push(Diagnostic::error(
                "`Parallel.Reduce` requires an array argument",
                values.span,
            ));
            self.expression(identity);
            self.expression(accumulate_argument);
            self.expression(combine_argument);
            return Type::Unknown;
        };
        if !transferable(&element_type) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`Parallel.Reduce` over `{}[]` requires a scalar element type in this version",
                        element_type.display()
                    ),
                    values.span,
                )
                .with_help("Parallel.Reduce currently supports only scalar elements: bool, char, integers, floats"),
            );
            self.expression(identity);
            self.expression(accumulate_argument);
            self.expression(combine_argument);
            return Type::Unknown;
        }
        let identity_type = self.expression(identity);
        if !transferable(&identity_type) {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`Parallel.Reduce` identity of type `{}` cannot cross a worker boundary in this version",
                        identity_type.display()
                    ),
                    identity.span,
                )
                .with_help("the identity must be a scalar value: bool, char, integers, floats"),
            );
            self.expression(accumulate_argument);
            self.expression(combine_argument);
            return Type::Unknown;
        }
        let Some(accumulate) = self.resolve_reduce_operator_target(
            accumulate_argument,
            "Accumulate",
            &[identity_type.clone(), (*element_type).clone()],
            &identity_type,
        ) else {
            self.expression(combine_argument);
            return Type::Unknown;
        };
        let Some(combine) = self.resolve_reduce_operator_target(
            combine_argument,
            "Combine",
            &[identity_type.clone(), identity_type.clone()],
            &identity_type,
        ) else {
            return Type::Unknown;
        };
        self.model.parallel_reduce.insert(
            self.model_key(span),
            ResolvedParallelReduce {
                accumulate: accumulate.key,
                combine: combine.key,
            },
        );
        identity_type
    }

    /// Resolve a `Parallel.Reduce` `Accumulate`/`Combine` argument to the
    /// concrete, zero-capture free function or static method it directly
    /// names, with exactly `expected_parameters` and exactly
    /// `expected_return`. Generalizes [`Self::resolve_parallel_body_target`]
    /// to a non-`void` return and two parameters: never a variable, an
    /// instance method, an async function, or any other expression shape.
    fn resolve_reduce_operator_target(
        &mut self,
        argument: &Expression,
        operator_name: &str,
        expected_parameters: &[Type],
        expected_return: &Type,
    ) -> Option<Callable> {
        let candidates = match &argument.kind {
            ExpressionKind::Name(name) => {
                let mut candidates = self
                    .context
                    .functions
                    .get(name)
                    .into_iter()
                    .flatten()
                    .filter(|candidate| candidate.is_static)
                    .cloned()
                    .collect::<Vec<_>>();
                candidates.extend(
                    self.methods
                        .get(name)
                        .into_iter()
                        .flatten()
                        .filter(|candidate| candidate.is_static)
                        .cloned(),
                );
                candidates
            }
            ExpressionKind::Member { object, name } => {
                let ExpressionKind::Name(type_name) = &object.kind else {
                    self.diagnostics.push(Diagnostic::error(
                        format!(
                            "a `Parallel.Reduce` {operator_name} must directly name a static method or free function"
                        ),
                        argument.span,
                    ));
                    return None;
                };
                let Some(info) = self.context.types.get(type_name) else {
                    self.diagnostics.push(Diagnostic::error(
                        format!("unknown type `{type_name}`"),
                        argument.span,
                    ));
                    return None;
                };
                info.methods
                    .get(name)
                    .into_iter()
                    .flatten()
                    .filter(|candidate| candidate.is_static)
                    .cloned()
                    .collect::<Vec<_>>()
            }
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "a `Parallel.Reduce` {operator_name} must directly name a static method or free function"
                    ),
                    argument.span,
                ));
                return None;
            }
        };
        let matching = candidates
            .into_iter()
            .filter(|candidate| {
                candidate.signature.result == *expected_return
                    && candidate.signature.parameters == expected_parameters
            })
            .collect::<Vec<_>>();
        match matching.len() {
            0 => {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "no static method or free function with signature `{}({})` is available for this `Parallel.Reduce` {operator_name}",
                            expected_return.display(),
                            expected_parameters
                                .iter()
                                .map(Type::display)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        argument.span,
                    )
                    .with_help(format!(
                        "{operator_name} must be a static method or free function taking exactly the expected parameters and returning the accumulator type"
                    )),
                );
                None
            }
            1 => matching.into_iter().next(),
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "this `Parallel.Reduce` {operator_name} is ambiguous among multiple candidates"
                    ),
                    argument.span,
                ));
                None
            }
        }
    }

    /// Resolve a `Parallel.For`/`Parallel.ForEach` body argument to the
    /// concrete, zero-capture free function or static method it directly
    /// names, with exactly `expected_parameters` and a `void` result. Mirrors
    /// [`Self::resolve_task_run_target`]: never a variable, an instance
    /// method, an async function, or any other expression shape.
    fn resolve_parallel_body_target(
        &mut self,
        argument: &Expression,
        expected_parameters: &[Type],
    ) -> Option<Callable> {
        let candidates = match &argument.kind {
            ExpressionKind::Name(name) => {
                let mut candidates = self
                    .context
                    .functions
                    .get(name)
                    .into_iter()
                    .flatten()
                    .filter(|candidate| candidate.is_static)
                    .cloned()
                    .collect::<Vec<_>>();
                candidates.extend(
                    self.methods
                        .get(name)
                        .into_iter()
                        .flatten()
                        .filter(|candidate| candidate.is_static)
                        .cloned(),
                );
                candidates
            }
            ExpressionKind::Member { object, name } => {
                let ExpressionKind::Name(type_name) = &object.kind else {
                    self.diagnostics.push(Diagnostic::error(
                        "a `Parallel` body must directly name a static method or free function",
                        argument.span,
                    ));
                    return None;
                };
                let Some(info) = self.context.types.get(type_name) else {
                    self.diagnostics.push(Diagnostic::error(
                        format!("unknown type `{type_name}`"),
                        argument.span,
                    ));
                    return None;
                };
                info.methods
                    .get(name)
                    .into_iter()
                    .flatten()
                    .filter(|candidate| candidate.is_static)
                    .cloned()
                    .collect::<Vec<_>>()
            }
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    "a `Parallel` body must directly name a static method or free function",
                    argument.span,
                ));
                return None;
            }
        };
        let matching = candidates
            .into_iter()
            .filter(|candidate| {
                candidate.signature.result == Type::Void
                    && candidate.signature.parameters == expected_parameters
            })
            .collect::<Vec<_>>();
        match matching.len() {
            0 => {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "no static method or free function with signature `void({})` is available for this `Parallel` body",
                            expected_parameters
                                .iter()
                                .map(Type::display)
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        argument.span,
                    )
                    .with_help("the body must be a static method or free function taking exactly one matching parameter and returning void"),
                );
                None
            }
            1 => matching.into_iter().next(),
            _ => {
                self.diagnostics.push(Diagnostic::error(
                    "this `Parallel` body is ambiguous among multiple candidates",
                    argument.span,
                ));
                None
            }
        }
    }

    fn check_arguments(&mut self, signature: &Signature, arguments: &[Argument], span: Span) {
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

fn bind_arguments(candidate: &Callable, arguments: &[Argument]) -> Option<Vec<usize>> {
    let mut occupied = vec![false; candidate.signature.parameters.len()];
    let mut mapping = Vec::with_capacity(arguments.len());
    let mut next = 0usize;
    let mut saw_named = false;
    for argument in arguments {
        let parameter = if let Some(name) = &argument.name {
            saw_named = true;
            candidate
                .parameter_names
                .iter()
                .position(|parameter| parameter == name)?
        } else {
            if saw_named {
                return None;
            }
            while next < occupied.len() && occupied[next] {
                next += 1;
            }
            if next == occupied.len() {
                return None;
            }
            let parameter = next;
            next += 1;
            parameter
        };
        if std::mem::replace(&mut occupied[parameter], true) {
            return None;
        }
        mapping.push(parameter);
    }
    if occupied
        .iter()
        .enumerate()
        .any(|(parameter, supplied)| !supplied && candidate.defaults[parameter].is_none())
    {
        return None;
    }
    Some(mapping)
}

fn bind_enum_arguments(fields: &[(String, Type)], arguments: &[Argument]) -> Option<Vec<usize>> {
    if arguments.len() != fields.len() {
        return None;
    }
    let mut occupied = vec![false; fields.len()];
    let mut order = Vec::with_capacity(arguments.len());
    let mut next = 0usize;
    let mut saw_named = false;
    for argument in arguments {
        let parameter = if let Some(name) = &argument.name {
            saw_named = true;
            fields.iter().position(|(field, _)| field == name)?
        } else {
            if saw_named {
                return None;
            }
            let parameter = next;
            next += 1;
            parameter
        };
        if std::mem::replace(&mut occupied[parameter], true) {
            return None;
        }
        order.push(parameter);
    }
    Some(order)
}

fn diagnose_enum_arguments(
    enum_name: &str,
    case_name: &str,
    fields: &[(String, Type)],
    arguments: &[Argument],
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if arguments.len() != fields.len() {
        diagnostics.push(Diagnostic::error(
            format!(
                "enum case `{enum_name}.{case_name}` expects {} argument(s), found {}",
                fields.len(),
                arguments.len()
            ),
            span,
        ));
    }
    let mut occupied = vec![false; fields.len()];
    let mut next = 0usize;
    for argument in arguments {
        let parameter = if let Some(name) = &argument.name {
            let Some(parameter) = fields.iter().position(|(field, _)| field == name) else {
                diagnostics.push(Diagnostic::error(
                    format!("unknown enum payload argument `{name}`"),
                    argument.span,
                ));
                continue;
            };
            parameter
        } else {
            while next < occupied.len() && occupied[next] {
                next += 1;
            }
            if next == occupied.len() {
                continue;
            }
            let parameter = next;
            next += 1;
            parameter
        };
        if std::mem::replace(&mut occupied[parameter], true) {
            diagnostics.push(Diagnostic::error(
                format!(
                    "enum payload `{}` is supplied more than once",
                    fields[parameter].0
                ),
                argument.span,
            ));
        }
    }
    for (index, supplied) in occupied.into_iter().enumerate() {
        if !supplied {
            diagnostics.push(Diagnostic::error(
                format!("missing enum payload argument `{}`", fields[index].0),
                span,
            ));
        }
    }
}

/// Structural check for `Task.Run(...)`, mirroring `logging_level`: `Task`
/// is recognized as this well-known API surface only in exactly this shape,
/// never by scanning source text elsewhere.
pub(super) fn is_task_run_callee(callee: &Expression) -> bool {
    matches!(
        &callee.kind,
        ExpressionKind::Member { object, name }
            if name == "Run"
                && matches!(&object.kind, ExpressionKind::Name(object) if object == "Task")
    )
}

pub(super) fn is_task_wait_all_callee(callee: &Expression) -> bool {
    matches!(
        &callee.kind,
        ExpressionKind::Member { object, name }
            if name == "WaitAll"
                && matches!(&object.kind, ExpressionKind::Name(object) if object == "Task")
    )
}

pub(super) fn is_task_cancellation_requested_callee(callee: &Expression) -> bool {
    matches!(
        &callee.kind,
        ExpressionKind::Member { object, name }
            if name == "IsCancellationRequested"
                && matches!(&object.kind, ExpressionKind::Name(object) if object == "Task")
    )
}

/// Structural check for `Parallel.For(...)`, mirroring [`is_task_run_callee`]:
/// `Parallel` is reserved (see `semantic::validate_no_reserved_type_names`),
/// so this shape alone identifies the intrinsic, never a user declaration or
/// an unrelated `For`/`ForEach` function elsewhere.
pub(super) fn is_parallel_for_callee(callee: &Expression) -> bool {
    matches!(
        &callee.kind,
        ExpressionKind::Member { object, name }
            if name == "For"
                && matches!(&object.kind, ExpressionKind::Name(object) if object == "Parallel")
    )
}

/// Structural check for `Parallel.ForEach(...)`, mirroring [`is_task_run_callee`].
pub(super) fn is_parallel_for_each_callee(callee: &Expression) -> bool {
    matches!(
        &callee.kind,
        ExpressionKind::Member { object, name }
            if name == "ForEach"
                && matches!(&object.kind, ExpressionKind::Name(object) if object == "Parallel")
    )
}

/// Structural check for `Parallel.Reduce(...)`, mirroring [`is_task_run_callee`].
pub(super) fn is_parallel_reduce_callee(callee: &Expression) -> bool {
    matches!(
        &callee.kind,
        ExpressionKind::Member { object, name }
            if name == "Reduce"
                && matches!(&object.kind, ExpressionKind::Name(object) if object == "Parallel")
    )
}

/// Whether a value of type `type_` can cross an Aster worker boundary
/// (`Task.Run`, `Parallel.For`/`ForEach`, or an async frame slot) as owned
/// data in this version.
///
/// Derives from `aster_types::Primitive::is_worker_transferable` rather than
/// maintaining a second hand-written list: only types with a primitive
/// behind them (`Type::primitive`) are even candidates, so `void`, arrays,
/// classes, interfaces, enums, structs, and `Task<T>` itself are excluded
/// automatically (`primitive()` returns `None` for all of them) rather than
/// by name here. Among primitives, `decimal` and `string` are excluded
/// because neither has a fixed-width, arena-free ABI yet (see
/// `Primitive::is_worker_transferable`), even though both are otherwise
/// recognized, ordinary value types.
pub(super) fn transferable(type_: &Type) -> bool {
    type_
        .primitive()
        .is_some_and(Primitive::is_worker_transferable)
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
