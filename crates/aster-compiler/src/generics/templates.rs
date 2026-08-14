use super::{
    DeclarationFacts, DeclarationKind, Diagnostic, FunctionDeclaration, HashMap, Item, Member,
    Module, Monomorphizer, TypeDeclaration, TypeName, TypeParameter,
};

#[derive(Clone)]
pub(super) enum GenericTypeTemplate {
    Class(TypeDeclaration),
    Struct(TypeDeclaration),
    Interface(TypeDeclaration),
}

impl GenericTypeTemplate {
    pub(super) fn declaration(&self) -> &TypeDeclaration {
        match self {
            Self::Class(value) | Self::Struct(value) | Self::Interface(value) => value,
        }
    }

    pub(super) fn into_item(self, declaration: TypeDeclaration) -> Item {
        match self {
            Self::Class(_) => Item::Class(declaration),
            Self::Struct(_) => Item::Struct(declaration),
            Self::Interface(_) => Item::Interface(declaration),
        }
    }
}

/// `description` carries its own article, so kinds like `an enum` read
/// correctly next to `a class`.
fn not_an_interface(name: &str, description: &str, span: aster_diagnostics::Span) -> Diagnostic {
    Diagnostic::error(
        format!("`{name}` is not an interface; it is {description}"),
        span,
    )
    .with_help("generic constraints accept only interfaces in this subset")
}

/// How to describe a constraint that names a built-in type rather than a
/// declared one. `where T : int` must report that `int` is not an interface,
/// not that it is unknown.
fn builtin_constraint_description(base: &str) -> Option<&'static str> {
    if base == "void" {
        return Some("a built-in type");
    }
    if crate::primitives::from_name(base).is_some() {
        return Some("a primitive type");
    }
    matches!(base, "Task" | "List" | "Dictionary" | "Parallel")
        .then_some("a reserved built-in type")
}

impl Monomorphizer {
    #[allow(clippy::too_many_lines)]
    pub(super) fn new(module: &Module) -> Self {
        let mut templates = HashMap::new();
        let mut type_templates = HashMap::new();
        let mut enum_templates = HashMap::new();
        let mut returns = HashMap::new();
        let mut fields = HashMap::new();
        let mut methods = HashMap::new();
        let mut method_signatures: HashMap<(String, String), Vec<Vec<String>>> = HashMap::new();
        let mut method_templates: HashMap<(String, String), Vec<FunctionDeclaration>> =
            HashMap::new();
        let mut function_kinds = HashMap::new();
        let mut diagnostics = Vec::new();
        let mut declarations = HashMap::new();
        let mut class_interfaces = HashMap::new();
        // The constraint inventory is built in its own pass, ahead of template
        // discovery: a call site requesting a specialization can appear in
        // `module.items` before the class it names, so satisfaction cannot rely
        // on the interleaved per-item walk in `run`.
        for item in &module.items {
            let (kind, name, type_parameters) = match item {
                Item::Class(value) => (DeclarationKind::Class, &value.name, &value.type_parameters),
                Item::Struct(value) => {
                    (DeclarationKind::Struct, &value.name, &value.type_parameters)
                }
                Item::Interface(value) => (
                    DeclarationKind::Interface,
                    &value.name,
                    &value.type_parameters,
                ),
                Item::Enum(value) => (DeclarationKind::Enum, &value.name, &value.type_parameters),
                Item::Function(_) | Item::Variable(_) => continue,
            };
            declarations.insert(
                name.clone(),
                DeclarationFacts {
                    kind,
                    arity: type_parameters.len(),
                },
            );
            if let Item::Class(value) = item
                && !value.interfaces.is_empty()
            {
                class_interfaces.insert(
                    value.name.clone(),
                    value
                        .interfaces
                        .iter()
                        .map(|interface| interface.name.clone())
                        .collect(),
                );
            }
        }
        for item in &module.items {
            match item {
                Item::Function(function) => {
                    let generic = !function.type_parameters.is_empty();
                    if function_kinds
                        .insert(function.name.clone(), generic)
                        .is_some_and(|previous| previous || generic)
                    {
                        diagnostics.push(
                            Diagnostic::error(
                                format!("duplicate function `{}`", function.name),
                                function.span,
                            )
                            .with_help("generic function overloads are not implemented"),
                        );
                    }
                    if function.type_parameters.is_empty() {
                        returns.insert(function.name.clone(), function.return_type.name.clone());
                    } else {
                        templates.insert(function.name.clone(), function.clone());
                    }
                }
                Item::Class(declaration)
                | Item::Struct(declaration)
                | Item::Interface(declaration) => {
                    if !declaration.type_parameters.is_empty() {
                        let template = match item {
                            Item::Class(value) => GenericTypeTemplate::Class(value.clone()),
                            Item::Struct(value) => GenericTypeTemplate::Struct(value.clone()),
                            Item::Interface(value) => GenericTypeTemplate::Interface(value.clone()),
                            _ => unreachable!(),
                        };
                        if type_templates
                            .insert(declaration.name.clone(), template)
                            .is_some()
                        {
                            diagnostics.push(Diagnostic::error(
                                format!("duplicate generic type `{}`", declaration.name),
                                declaration.span,
                            ));
                        }
                        continue;
                    }
                    for member in &declaration.members {
                        match member {
                            Member::Field(field) => {
                                fields.insert(
                                    (declaration.name.clone(), field.name.clone()),
                                    field.type_ref.name.clone(),
                                );
                            }
                            Member::Method(method) => {
                                let key = (declaration.name.clone(), method.name.clone());
                                if method.type_parameters.is_empty() {
                                    methods.insert(key.clone(), method.return_type.name.clone());
                                    method_signatures.entry(key).or_default().push(
                                        method
                                            .parameters
                                            .iter()
                                            .map(|parameter| parameter.type_ref.name.clone())
                                            .collect(),
                                    );
                                } else {
                                    method_templates
                                        .entry(key)
                                        .or_default()
                                        .push(method.clone());
                                }
                            }
                            Member::Property(property) => {
                                fields.insert(
                                    (declaration.name.clone(), property.name.clone()),
                                    property.type_ref.name.clone(),
                                );
                            }
                        }
                    }
                }
                Item::Enum(declaration)
                    if !declaration.type_parameters.is_empty()
                        && enum_templates
                            .insert(declaration.name.clone(), declaration.clone())
                            .is_some() =>
                {
                    diagnostics.push(Diagnostic::error(
                        format!("duplicate generic enum `{}`", declaration.name),
                        declaration.span,
                    ));
                }
                _ => {}
            }
        }
        Self {
            templates,
            type_templates,
            enum_templates,
            returns,
            fields,
            methods,
            method_signatures,
            method_templates,
            method_cache: HashMap::new(),
            method_active: Vec::new(),
            generated_methods: HashMap::new(),
            cache: HashMap::new(),
            active: Vec::new(),
            generated: Vec::new(),
            type_cache: HashMap::new(),
            type_active: Vec::new(),
            generated_types: Vec::new(),
            declarations,
            class_interfaces,
            diagnostics,
        }
    }

    /// Template well-formedness, including `where` constraints.
    ///
    /// Templates live in hash maps, so diagnostics are collected locally and
    /// sorted by source span before emission. Without that, a program with two
    /// bad templates reports them in an unpredictable order.
    pub(super) fn validate_templates(&mut self) {
        let mut reported = Vec::new();
        for template in self.templates.values() {
            self.validate_type_parameters(&template.type_parameters, &mut reported);
        }
        for ((owner, _), templates) in &self.method_templates {
            for method in templates {
                self.validate_type_parameters(&method.type_parameters, &mut reported);
                if self
                    .declarations
                    .get(owner)
                    .is_some_and(|facts| facts.kind == DeclarationKind::Interface)
                {
                    reported.push(
                        Diagnostic::error(
                            "generic interface methods are not implemented",
                            method.span,
                        )
                        .with_help(
                            "use a generic interface specialization or a generic class/struct method",
                        ),
                    );
                }
            }
        }
        for template in self.type_templates.values() {
            let declaration = template.declaration();
            if declaration.is_static {
                reported.push(
                    Diagnostic::error(
                        "generic static classes are not implemented",
                        declaration.span,
                    )
                    .with_help("use a generic namespace function or an instantiable generic class"),
                );
            }
            self.validate_type_parameters(&declaration.type_parameters, &mut reported);
            for member in &declaration.members {
                if let Member::Method(method) = member {
                    if !method.type_parameters.is_empty() {
                        self.validate_type_parameters(&method.type_parameters, &mut reported);
                        if matches!(template, GenericTypeTemplate::Interface(_)) {
                            reported.push(
                                Diagnostic::error(
                                    "generic interface methods are not implemented",
                                    method.span,
                                )
                                .with_help(
                                    "use a generic interface specialization or a generic class/struct method",
                                ),
                            );
                        }
                    }
                    if method.is_static {
                        reported.push(
                            Diagnostic::error(
                                "static methods on generic types are not implemented",
                                method.span,
                            )
                            .with_help("use an instance method or a generic namespace function"),
                        );
                    }
                }
            }
        }
        for declaration in self.enum_templates.values() {
            self.validate_type_parameters(&declaration.type_parameters, &mut reported);
        }
        reported.sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.span.end));
        self.diagnostics.append(&mut reported);
    }

    fn validate_type_parameters(
        &self,
        parameters: &[TypeParameter],
        reported: &mut Vec<Diagnostic>,
    ) {
        let mut seen = HashMap::new();
        for parameter in parameters {
            if seen
                .insert(parameter.name.as_str(), parameter.span)
                .is_some()
            {
                reported.push(
                    Diagnostic::error(
                        format!("duplicate type parameter `{}`", parameter.name),
                        parameter.span,
                    )
                    .with_help("give every type parameter a unique name"),
                );
            }
            self.validate_constraints(parameter, reported);
        }
    }

    /// Constraint well-formedness for one type parameter. Constraints remain
    /// nominal, but may name a correctly closed generic interface and may use
    /// any type parameter from the declaration in that closed shape.
    fn validate_constraints(&self, parameter: &TypeParameter, reported: &mut Vec<Diagnostic>) {
        let mut seen: Vec<&str> = Vec::new();
        for constraint in &parameter.constraints {
            let name = constraint.name.as_str();
            if seen.contains(&name) {
                reported.push(
                    Diagnostic::error(format!("duplicate constraint `{name}`"), constraint.span)
                        .with_help("remove the repeated interface from the `where` clause"),
                );
                continue;
            }
            seen.push(name);
            let Some(type_name) = TypeName::parse(name) else {
                reported.push(
                    Diagnostic::error(
                        format!("malformed constraint type `{name}`"),
                        constraint.span,
                    )
                    .with_help("name a single non-generic interface"),
                );
                continue;
            };
            if type_name.array {
                reported.push(not_an_interface(name, "an array type", constraint.span));
                continue;
            }
            let Some(facts) = self.declarations.get(&type_name.base) else {
                if let Some(description) = builtin_constraint_description(&type_name.base) {
                    reported.push(not_an_interface(name, description, constraint.span));
                } else {
                    reported.push(
                        Diagnostic::error(
                            format!("unknown constraint type `{name}`"),
                            constraint.span,
                        )
                        .with_help("declare the interface or add a using for its namespace"),
                    );
                }
                continue;
            };
            if facts.kind != DeclarationKind::Interface {
                reported.push(not_an_interface(
                    name,
                    facts.kind.describe(),
                    constraint.span,
                ));
                continue;
            }
            if type_name.arguments.len() != facts.arity {
                reported.push(
                    Diagnostic::error(
                        format!(
                            "generic interface constraint `{}` expects {} type argument(s), found {}",
                            type_name.base,
                            facts.arity,
                            type_name.arguments.len()
                        ),
                        constraint.span,
                    )
                    .with_help("provide every required type argument in the constraint"),
                );
            }
        }
    }
}
