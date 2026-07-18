use super::{Diagnostic, HashMap, Item, Member, Module, Monomorphizer, TypeDeclaration};

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

impl Monomorphizer {
    #[allow(clippy::too_many_lines)]
    pub(super) fn new(module: &Module) -> Self {
        let mut templates = HashMap::new();
        let mut type_templates = HashMap::new();
        let mut enum_templates = HashMap::new();
        let mut returns = HashMap::new();
        let mut fields = HashMap::new();
        let mut methods = HashMap::new();
        let mut function_kinds = HashMap::new();
        let mut diagnostics = Vec::new();
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
                                methods.insert(
                                    (declaration.name.clone(), method.name.clone()),
                                    method.return_type.name.clone(),
                                );
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
            cache: HashMap::new(),
            active: Vec::new(),
            generated: Vec::new(),
            type_cache: HashMap::new(),
            type_active: Vec::new(),
            generated_types: Vec::new(),
            diagnostics,
        }
    }

    pub(super) fn validate_templates(&mut self) {
        for template in self.templates.values() {
            let mut seen = HashMap::new();
            for parameter in &template.type_parameters {
                if seen
                    .insert(parameter.name.as_str(), parameter.span)
                    .is_some()
                {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("duplicate type parameter `{}`", parameter.name),
                            parameter.span,
                        )
                        .with_help("give every type parameter a unique name"),
                    );
                }
            }
        }
        for template in self.type_templates.values() {
            let declaration = template.declaration();
            if declaration.is_static {
                self.diagnostics.push(
                    Diagnostic::error(
                        "generic static classes are not implemented",
                        declaration.span,
                    )
                    .with_help("use a generic namespace function or an instantiable generic class"),
                );
            }
            let mut seen = HashMap::new();
            for parameter in &declaration.type_parameters {
                if seen
                    .insert(parameter.name.as_str(), parameter.span)
                    .is_some()
                {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("duplicate type parameter `{}`", parameter.name),
                            parameter.span,
                        )
                        .with_help("give every type parameter a unique name"),
                    );
                }
            }
            for member in &declaration.members {
                if let Member::Method(method) = member {
                    if matches!(template, GenericTypeTemplate::Struct(_)) {
                        self.diagnostics.push(
                            Diagnostic::error(
                                "struct methods are not executable yet, including on generic structs",
                                method.span,
                            )
                            .with_help("keep the generic struct as data and use a namespace function"),
                        );
                    }
                    if !method.type_parameters.is_empty() {
                        self.diagnostics.push(
                            Diagnostic::error(
                                "generic methods are not implemented; methods may use only their owner type parameters",
                                method.span,
                            )
                            .with_help("move the additional type parameters to a namespace function"),
                        );
                    }
                    if method.is_static {
                        self.diagnostics.push(
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
            let mut seen = HashMap::new();
            for parameter in &declaration.type_parameters {
                if seen
                    .insert(parameter.name.as_str(), parameter.span)
                    .is_some()
                {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("duplicate type parameter `{}`", parameter.name),
                            parameter.span,
                        )
                        .with_help("give every type parameter a unique name"),
                    );
                }
            }
        }
    }
}
