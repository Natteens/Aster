use super::{
    AstVisitorMut, Diagnostic, Expression, ExpressionKind, HashMap, Item, Member, Monomorphizer,
    SwitchCase, TypeName, TypeRef, TypeSubstituter, Write, infer_type, substitute_name,
    substitutions, walk_expression_mut, walk_switch_case_mut,
};

impl Monomorphizer {
    fn concretize_type(&mut self, type_ref: &mut TypeRef) {
        type_ref.name = self.concretize_type_name(&type_ref.name, type_ref.span);
    }

    fn concretize_type_name(&mut self, name: &str, span: aster_diagnostics::Span) -> String {
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
    fn instantiate_type(
        &mut self,
        name: &str,
        concrete: &[TypeName],
        span: aster_diagnostics::Span,
    ) -> String {
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
        GenericTypeConcretizer::new(self).visit_type_declaration_mut(&mut declaration);
        self.analyze_type_declaration(&mut declaration);
        self.type_active.pop();
        self.generated_types.push(template.into_item(declaration));
        specialized
    }

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
