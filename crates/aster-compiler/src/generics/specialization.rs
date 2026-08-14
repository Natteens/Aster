use super::{
    AstVisitorMut, Diagnostic, Expression, ExpressionKind, HashMap, Item, Member,
    MethodSpecializationKey, MethodTemplateKey, Monomorphizer, SwitchCase, SwitchExpressionCase,
    TypeName, TypeRef, TypeSubstituter, Write, infer_type, substitute_name, substitutions,
    walk_expression_mut, walk_switch_case_mut, walk_switch_expression_case_mut,
};

impl Monomorphizer {
    fn concretize_type(&mut self, type_ref: &mut TypeRef) {
        type_ref.name = self.concretize_type_name(&type_ref.name, type_ref.span);
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn concretize_type_name(
        &mut self,
        name: &str,
        span: aster_diagnostics::Span,
    ) -> String {
        let Some(mut type_name) = TypeName::parse(name) else {
            self.diagnostics.push(Diagnostic::error(
                format!("malformed generic type `{name}`"),
                span,
            ));
            return name.to_owned();
        };
        for argument in &mut type_name.arguments {
            *argument = TypeName::parse(&self.concretize_type_name(&argument.to_string(), span))
                .unwrap_or_else(|| argument.clone());
        }
        if type_name.base == "Task" {
            // `Task` is reserved (see `semantic::validate_no_reserved_type_names`),
            // so `Task<T>` is always the compiler intrinsic (`hir::Type::Task`),
            // never a user or stdlib generic template: it never goes through
            // template-arity checking or monomorphization here. Semantic
            // analysis resolves it directly from this reconstructed name.
            return type_name.to_string();
        }
        if type_name.base == "List" {
            // `List` is reserved (see `semantic::validate_no_reserved_type_names`),
            // so `List<T>` is always the compiler intrinsic (`hir::Type::List`),
            // never a user or stdlib generic template. Unlike `Task<T>`, the
            // element `T` is validated here (arity, `void`, `decimal`)
            // because semantic resolution collapses any invalid element to
            // `Unknown` without a dedicated diagnostic of its own.
            if type_name.arguments.len() != 1 {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`List` expects 1 type argument, found {}",
                            type_name.arguments.len()
                        ),
                        span,
                    )
                    .with_help("write `List<T>` with exactly one concrete element type"),
                );
                return type_name.to_string();
            }
            let element = &type_name.arguments[0];
            if element.arguments.is_empty() && !element.array {
                if element.base == "void" {
                    self.diagnostics.push(
                        Diagnostic::error("`List<void>` is not supported", span)
                            .with_help("use a concrete, non-`void` element type"),
                    );
                    return type_name.to_string();
                }
                if element.base == "decimal" {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "`List<decimal>` cannot be used until `decimal` is executable",
                            span,
                        )
                        .with_help("use an executable element type"),
                    );
                    return type_name.to_string();
                }
            }
            return type_name.to_string();
        }
        if type_name.base == "Dictionary" {
            if type_name.arguments.len() != 2 {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "`Dictionary` expects 2 type arguments, found {}",
                        type_name.arguments.len()
                    ),
                    span,
                ));
                return type_name.to_string();
            }
            let key = &type_name.arguments[0];
            let supported_key = !key.array
                && key.arguments.is_empty()
                && matches!(
                    key.base.as_str(),
                    "bool"
                        | "char"
                        | "sbyte"
                        | "byte"
                        | "short"
                        | "ushort"
                        | "int"
                        | "uint"
                        | "long"
                        | "ulong"
                        | "string"
                );
            if !supported_key {
                self.diagnostics.push(Diagnostic::error(
                    format!("type `{key}` is not supported as a Dictionary key in ASTER 1.0"),
                    span,
                ));
            }
            let value = &type_name.arguments[1];
            if value.arguments.is_empty() && !value.array && value.base == "void" {
                self.diagnostics.push(Diagnostic::error(
                    "`Dictionary<K, void>` is not supported",
                    span,
                ));
            }
            return type_name.to_string();
        }
        let template_arity = self
            .type_templates
            .get(&type_name.base)
            .map(|template| template.declaration().type_parameters.len())
            .or_else(|| {
                self.enum_templates
                    .get(&type_name.base)
                    .map(|template| template.type_parameters.len())
            });
        let Some(expected) = template_arity else {
            if !type_name.arguments.is_empty() {
                self.diagnostics.push(
                    Diagnostic::error(format!("type `{}` is not generic", type_name.base), span)
                        .with_help("remove the type arguments"),
                );
            }
            return type_name.to_string();
        };
        if type_name.arguments.len() != expected {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "generic type `{}` expects {expected} type argument(s), found {}",
                        type_name.base,
                        type_name.arguments.len()
                    ),
                    span,
                )
                .with_help("provide every required concrete type argument"),
            );
            return type_name.to_string();
        }
        if type_name.array {
            let mut element = type_name.clone();
            element.array = false;
            let concrete = self.instantiate_type(&element.base, &element.arguments, span);
            return format!("{concrete}[]");
        }
        self.instantiate_type(&type_name.base, &type_name.arguments, span)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn instantiate_type(
        &mut self,
        name: &str,
        concrete: &[TypeName],
        span: aster_diagnostics::Span,
    ) -> String {
        // Constraints are proven before the cache is consulted, so a satisfied
        // request cannot install an entry that silences a later bad one, and
        // every request path reaching this function is covered.
        let parameters = self
            .type_templates
            .get(name)
            .map(|template| template.declaration().type_parameters.clone())
            .or_else(|| {
                self.enum_templates
                    .get(name)
                    .map(|template| template.type_parameters.clone())
            })
            .unwrap_or_default();
        if parameters
            .iter()
            .any(|parameter| !parameter.constraints.is_empty())
        {
            let arguments = concrete.iter().map(ToString::to_string).collect::<Vec<_>>();
            if !self.check_constraints(&parameters, &arguments, span) {
                return TypeName {
                    base: name.to_owned(),
                    arguments: concrete.to_vec(),
                    array: false,
                }
                .to_string();
            }
        }
        let key = (name.to_owned(), concrete.to_vec());
        if let Some(specialized) = self.type_cache.get(&key) {
            return specialized.clone();
        }
        if self
            .type_active
            .iter()
            .any(|(active, arguments)| active == name && arguments != concrete)
        {
            self.diagnostics.push(
                Diagnostic::error(
                    format!("generic type `{name}` creates an infinitely expanding specialization"),
                    span,
                )
                .with_help(
                    "make recursive fields reuse the same closed type or add reference indirection",
                ),
            );
            return TypeName {
                base: name.to_owned(),
                arguments: concrete.to_vec(),
                array: false,
            }
            .to_string();
        }
        let specialized = TypeName {
            base: name.to_owned(),
            arguments: concrete.to_vec(),
            array: false,
        }
        .to_string();
        // Reserve the closed identity before walking its clone. Exact self-recursion then reuses
        // the cache, while `type_active` still rejects a changed argument set for this template.
        self.type_cache.insert(key.clone(), specialized.clone());
        self.type_active.push(key);
        if let Some(template) = self.enum_templates.get(name).cloned() {
            let parameter_names = template
                .type_parameters
                .iter()
                .map(|parameter| parameter.name.clone())
                .collect::<Vec<_>>();
            let substitutions = substitutions(
                &parameter_names,
                &concrete.iter().map(ToString::to_string).collect::<Vec<_>>(),
            );
            let mut declaration = template;
            declaration.name.clone_from(&specialized);
            declaration.type_parameters.clear();
            TypeSubstituter::new(&substitutions).visit_enum_declaration_mut(&mut declaration);
            GenericTypeConcretizer::new(self).visit_enum_declaration_mut(&mut declaration);
            self.type_active.pop();
            self.generated_types.push(Item::Enum(declaration));
            return specialized;
        }
        let template = self.type_templates[name].clone();
        let parameter_names = template
            .declaration()
            .type_parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let substitutions = substitutions(
            &parameter_names,
            &concrete.iter().map(ToString::to_string).collect::<Vec<_>>(),
        );
        let mut declaration = template.declaration().clone();
        declaration.name.clone_from(&specialized);
        declaration.type_parameters.clear();
        for member in &mut declaration.members {
            if let Member::Method(method) = member
                && method.constructor
            {
                method.name.clone_from(&specialized);
            }
        }
        TypeSubstituter::new(&substitutions).visit_type_declaration_mut(&mut declaration);
        self.extract_generic_methods(&mut declaration);
        GenericTypeConcretizer::new(self).visit_type_declaration_mut(&mut declaration);
        self.analyze_type_declaration(&mut declaration);
        self.type_active.pop();
        self.generated_types.push(template.into_item(declaration));
        specialized
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn instantiate(
        &mut self,
        name: &str,
        explicit: &[TypeRef],
        arguments: &[String],
        span: aster_diagnostics::Span,
    ) -> Option<(String, String)> {
        let template = self.templates[name].clone();
        let parameter_names = template
            .type_parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let concrete: Vec<String> = if explicit.is_empty() {
            let mut inferred = HashMap::new();
            for (parameter, actual) in template.parameters.iter().zip(arguments) {
                infer_type(
                    &parameter.type_ref.name,
                    actual,
                    &parameter_names,
                    &mut inferred,
                    span,
                    &mut self.diagnostics,
                );
            }
            let missing = parameter_names
                .iter()
                .find(|parameter| !inferred.contains_key(*parameter));
            if let Some(missing) = missing {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "cannot infer type parameter `{missing}` for generic function `{name}`"
                        ),
                        span,
                    )
                    .with_help("provide explicit type arguments"),
                );
                return None;
            }
            parameter_names
                .iter()
                .map(|parameter| inferred[parameter].clone())
                .collect()
        } else {
            if explicit.len() != parameter_names.len() {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "generic function `{name}` expects {} type argument(s), found {}",
                        parameter_names.len(),
                        explicit.len()
                    ),
                    span,
                ));
                return None;
            }
            explicit.iter().map(|type_| type_.name.clone()).collect()
        };
        if concrete.iter().any(String::is_empty) {
            self.diagnostics.push(Diagnostic::error(
                format!("cannot infer concrete types for generic function `{name}`"),
                span,
            ));
            return None;
        }
        // Proven before the cache lookup, so explicit, inferred and repeated
        // request sites are all held to the same contract.
        if !self.check_constraints(&template.type_parameters, &concrete, span) {
            return None;
        }
        let key = (name.to_owned(), concrete.clone());
        if let Some(specialized) = self.cache.get(&key) {
            let substitutions = substitutions(&parameter_names, &concrete);
            return Some((
                specialized.clone(),
                substitute_name(&template.return_type.name, &substitutions),
            ));
        }
        if self
            .active
            .iter()
            .any(|(active, types)| active == name && types != &concrete)
        {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "generic function `{name}` recursively creates a different specialization"
                    ),
                    span,
                )
                .with_help("keep recursive generic calls on the same concrete types"),
            );
            return None;
        }
        let specialized = specialized_name(name, &concrete);
        // As with types, an in-progress exact specialization is reusable by recursive calls;
        // `active` remains responsible for rejecting argument growth.
        self.cache.insert(key.clone(), specialized.clone());
        let substitutions = substitutions(&parameter_names, &concrete);
        let return_type = substitute_name(&template.return_type.name, &substitutions);
        let mut function = template;
        function.name.clone_from(&specialized);
        function.type_parameters.clear();
        TypeSubstituter::new(&substitutions).visit_function_declaration_mut(&mut function);
        GenericTypeConcretizer::new(self).visit_function_declaration_mut(&mut function);
        self.returns
            .insert(specialized.clone(), return_type.clone());
        self.active.push(key);
        self.function(&mut function);
        self.active.pop();
        self.generated.push(function);
        Some((specialized, return_type))
    }

    // Method specialization intentionally mirrors the adjacent free-function specialization
    // transaction from inference through cache installation and closed-body discovery.
    #[allow(clippy::too_many_lines)]
    pub(super) fn instantiate_method(
        &mut self,
        owner: &str,
        template: &aster_syntax::FunctionDeclaration,
        explicit: &[TypeRef],
        arguments: &[String],
        span: aster_diagnostics::Span,
    ) -> Option<(String, String)> {
        let parameter_names = template
            .type_parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let display_name = format!("{owner}.{}", template.name);
        let concrete = if explicit.is_empty() {
            let mut inferred = HashMap::new();
            for (parameter, actual) in template.parameters.iter().zip(arguments) {
                infer_type(
                    &parameter.type_ref.name,
                    actual,
                    &parameter_names,
                    &mut inferred,
                    span,
                    &mut self.diagnostics,
                );
            }
            if let Some(missing) = parameter_names
                .iter()
                .find(|parameter| !inferred.contains_key(*parameter))
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "cannot infer type parameter `{missing}` for generic method `{display_name}`"
                        ),
                        span,
                    )
                    .with_help("provide explicit type arguments"),
                );
                return None;
            }
            parameter_names
                .iter()
                .map(|parameter| inferred[parameter].clone())
                .collect::<Vec<_>>()
        } else {
            if explicit.len() != parameter_names.len() {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "generic method `{display_name}` expects {} type argument(s), found {}",
                        parameter_names.len(),
                        explicit.len()
                    ),
                    span,
                ));
                return None;
            }
            explicit
                .iter()
                .map(|type_| self.concretize_type_name(&type_.name, type_.span))
                .collect::<Vec<_>>()
        };
        if concrete.iter().any(String::is_empty) {
            self.diagnostics.push(Diagnostic::error(
                format!("cannot infer concrete types for generic method `{display_name}`"),
                span,
            ));
            return None;
        }
        if !self.check_constraints(&template.type_parameters, &concrete, span) {
            return None;
        }
        let template_key = MethodTemplateKey {
            owner: owner.to_owned(),
            declaration_start: template.span.start,
            parameters: template
                .parameters
                .iter()
                .map(|parameter| parameter.type_ref.name.clone())
                .collect(),
        };
        let key = MethodSpecializationKey {
            template: template_key,
            arguments: concrete.clone(),
        };
        let substitutions = substitutions(&parameter_names, &concrete);
        if let Some(specialized) = self.method_cache.get(&key) {
            return Some((
                specialized.clone(),
                self.methods
                    .get(&(owner.to_owned(), specialized.clone()))
                    .cloned()
                    .unwrap_or_else(|| substitute_name(&template.return_type.name, &substitutions)),
            ));
        }
        if self
            .method_active
            .iter()
            .any(|active| active.template == key.template && active.arguments != key.arguments)
        {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "generic method `{display_name}` recursively creates a different specialization"
                    ),
                    span,
                )
                .with_help("keep recursive generic method calls on the same concrete types"),
            );
            return None;
        }
        let base = format!("{}#method#{}#{}", owner, template.name, template.span.start);
        let specialized = specialized_name(&base, &concrete);
        self.method_cache.insert(key.clone(), specialized.clone());
        let diagnostics_before = self.diagnostics.len();
        let mut method = template.clone();
        method.name.clone_from(&specialized);
        method.type_parameters.clear();
        TypeSubstituter::new(&substitutions).visit_function_declaration_mut(&mut method);
        GenericTypeConcretizer::new(self).visit_function_declaration_mut(&mut method);
        let return_type = method.return_type.name.clone();
        let method_key = (owner.to_owned(), specialized.clone());
        self.methods.insert(method_key.clone(), return_type.clone());
        self.method_signatures.insert(
            method_key.clone(),
            vec![
                method
                    .parameters
                    .iter()
                    .map(|parameter| parameter.type_ref.name.clone())
                    .collect(),
            ],
        );
        let fields = self
            .fields
            .iter()
            .filter(|((field_owner, _), _)| field_owner == owner)
            .map(|((_, name), type_)| (name.clone(), type_.clone()))
            .collect::<HashMap<_, _>>();
        self.method_active.push(key.clone());
        self.analyze_method(owner, &mut method, &fields);
        self.method_active.pop();
        if self.diagnostics.len() != diagnostics_before {
            self.method_cache.remove(&key);
            self.methods.remove(&method_key);
            self.method_signatures.remove(&method_key);
            return None;
        }
        self.generated_methods
            .entry(owner.to_owned())
            .or_default()
            .push(method);
        Some((specialized, return_type))
    }
}

pub(super) struct GenericTypeConcretizer<'a> {
    monomorphizer: &'a mut Monomorphizer,
}

impl<'a> GenericTypeConcretizer<'a> {
    pub(super) fn new(monomorphizer: &'a mut Monomorphizer) -> Self {
        Self { monomorphizer }
    }
}

impl AstVisitorMut for GenericTypeConcretizer<'_> {
    fn visit_expression_mut(&mut self, expression: &mut Expression) {
        match &mut expression.kind {
            ExpressionKind::StructLiteral { type_name, .. }
            | ExpressionKind::NewObject { type_name, .. } => {
                *type_name = self
                    .monomorphizer
                    .concretize_type_name(type_name, expression.span);
            }
            ExpressionKind::Name(name)
                if TypeName::parse(name)
                    .is_some_and(|type_name| !type_name.arguments.is_empty()) =>
            {
                *name = self
                    .monomorphizer
                    .concretize_type_name(name, expression.span);
            }
            _ => {}
        }
        walk_expression_mut(self, expression);
    }

    fn visit_switch_case_mut(&mut self, case: &mut SwitchCase) {
        if let Some(owner) = &mut case.enum_name
            && TypeName::parse(owner).is_some_and(|type_name| !type_name.arguments.is_empty())
        {
            *owner = self.monomorphizer.concretize_type_name(owner, case.span);
        }
        walk_switch_case_mut(self, case);
    }

    fn visit_switch_expression_case_mut(&mut self, case: &mut SwitchExpressionCase) {
        if let Some(owner) = &mut case.enum_name
            && TypeName::parse(owner).is_some_and(|type_name| !type_name.arguments.is_empty())
        {
            *owner = self.monomorphizer.concretize_type_name(owner, case.span);
        }
        walk_switch_expression_case_mut(self, case);
    }

    fn visit_type_ref_mut(&mut self, type_ref: &mut TypeRef) {
        self.monomorphizer.concretize_type(type_ref);
    }
}

fn specialized_name(name: &str, concrete: &[String]) -> String {
    let encoded = concrete
        .iter()
        .map(|type_| {
            let bytes = type_
                .as_bytes()
                .iter()
                .fold(String::new(), |mut output, byte| {
                    write!(output, "{byte:02x}").expect("writing to String cannot fail");
                    output
                });
            format!("{}_{bytes}", type_.len())
        })
        .collect::<Vec<_>>()
        .join("#");
    format!("{name}#{encoded}")
}
