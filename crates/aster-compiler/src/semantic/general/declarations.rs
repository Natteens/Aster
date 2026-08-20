use super::{
    AccessorKind, Analyzer, Binding, Callable, Context, Diagnostic, EnumCaseInfo, Expression,
    ExpressionKind, Field, FunctionDeclaration, HashMap, HashSet, InterpolatedPart, Item, Member,
    Model, Module, Property, PropertyInfo, Signature, Span, Statement, Type, TypeDeclaration,
    TypeInfo, TypeKind, TypeName, TypeRef, Visibility, callable_key, resolve_type,
    resolve_type_readonly,
};

pub(super) fn collect_type_names(module: &Module, context: &mut Context) {
    for item in &module.items {
        match item {
            Item::Class(item) => {
                let info = context.types.entry(item.name.clone()).or_default();
                info.kind = Some(TypeKind::Class);
                info.is_static = item.is_static;
            }
            Item::Struct(item) => {
                context.types.entry(item.name.clone()).or_default().kind = Some(TypeKind::Struct);
            }
            Item::Interface(item) => {
                context.types.entry(item.name.clone()).or_default().kind =
                    Some(TypeKind::Interface);
            }
            Item::Enum(item) => {
                context.types.entry(item.name.clone()).or_default().kind = Some(TypeKind::Enum);
            }
            _ => {}
        }
    }
}

/// Locate the official `Result`/`Option` declarations once, keying off the core
/// standard-library namespace, and record their nominal identities. This is the
/// only place a type spelling is matched; after it, `?` compares the resolved
/// identity of the operand's enum against the stored official identity.
pub(super) fn discover_official_types(context: &mut Context) {
    let core_result = format!("{}::Result", crate::standard_library::CORE_NAMESPACE);
    let core_option = format!("{}::Option", crate::standard_library::CORE_NAMESPACE);
    let mut result = None;
    let mut option = None;
    for (name, info) in &context.types {
        if info.kind != Some(TypeKind::Enum) {
            continue;
        }
        let Some(base) = TypeName::parse(name).map(|type_name| type_name.base) else {
            continue;
        };
        if base == core_result {
            result = Some(base);
        } else if base == core_option {
            option = Some(base);
        }
    }
    context.official_result = result;
    context.official_option = option;
}

#[allow(clippy::too_many_lines)]
pub(super) fn collect_declarations(
    module: &Module,
    context: &mut Context,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &module.items {
        match item {
            Item::Class(declaration) => {
                validate_module_visibility(
                    declaration.visibility,
                    declaration.span,
                    "class",
                    diagnostics,
                );
                collect_type_members(declaration, TypeKind::Class, context, diagnostics);
            }
            Item::Struct(declaration) => {
                validate_module_visibility(
                    declaration.visibility,
                    declaration.span,
                    "struct",
                    diagnostics,
                );
                collect_type_members(declaration, TypeKind::Struct, context, diagnostics);
            }
            Item::Interface(declaration) => {
                validate_module_visibility(
                    declaration.visibility,
                    declaration.span,
                    "interface",
                    diagnostics,
                );
                collect_type_members(declaration, TypeKind::Interface, context, diagnostics);
            }
            Item::Enum(declaration) => {
                validate_module_visibility(
                    declaration.visibility,
                    declaration.span,
                    "enum",
                    diagnostics,
                );
                let mut names = HashSet::new();
                let mut cases = Vec::new();
                if declaration.cases.is_empty() {
                    diagnostics.push(Diagnostic::error(
                        format!("enum `{}` must declare at least one case", declaration.name),
                        declaration.span,
                    ));
                }
                for case in &declaration.cases {
                    if !names.insert(case.name.as_str()) {
                        diagnostics.push(Diagnostic::error(
                            format!("duplicate enum case `{}`", case.name),
                            case.span,
                        ));
                    }
                    let mut field_names = HashSet::new();
                    let mut fields = Vec::new();
                    for field in &case.fields {
                        if !field_names.insert(field.name.as_str()) {
                            diagnostics.push(Diagnostic::error(
                                format!("duplicate payload name `{}`", field.name),
                                field.span,
                            ));
                        }
                        let type_ = resolve_type(&field.type_ref, context, diagnostics);
                        if type_ == Type::Void {
                            diagnostics.push(Diagnostic::error(
                                "enum payload cannot have type `void`",
                                field.type_ref.span,
                            ));
                        }
                        fields.push((field.name.clone(), type_));
                    }
                    cases.push(EnumCaseInfo {
                        name: case.name.clone(),
                        fields,
                    });
                }
                context
                    .types
                    .entry(declaration.name.clone())
                    .or_default()
                    .enum_cases = cases;
            }
            Item::Function(function) => {
                validate_module_visibility(
                    function.visibility,
                    function.span,
                    "namespace function",
                    diagnostics,
                );
                let callable = Callable {
                    signature: signature(function, context, diagnostics),
                    visibility: function.visibility,
                    is_static: true,
                    is_foreign: function.is_foreign,
                    key: callable_key(&function.name, function.span.start, None, None),
                };
                validate_foreign_declaration(function, &callable.signature, diagnostics);
                let overloads = context.functions.entry(function.name.clone()).or_default();
                if overloads
                    .iter()
                    .any(|existing| existing.signature.parameters == callable.signature.parameters)
                {
                    diagnostics.push(
                        Diagnostic::error(
                            format!("duplicate overload `{}` with the same parameter types", function.name),
                            function.span,
                        )
                        .with_help("change the parameter types; return type alone cannot distinguish overloads"),
                    );
                }
                overloads.push(callable);
            }
            Item::Variable(variable) => {
                if let Some(visibility) = variable.visibility {
                    validate_module_visibility(
                        visibility,
                        variable.span,
                        "namespace variable",
                        diagnostics,
                    );
                }
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn collect_type_members(
    declaration: &TypeDeclaration,
    kind: TypeKind,
    context: &mut Context,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if declaration.is_static && !declaration.interfaces.is_empty() {
        diagnostics.push(
            Diagnostic::error(
                "a static class cannot implement instance interfaces",
                declaration.span,
            )
            .with_help("remove the interface list from the static class"),
        );
    }
    let mut fields = HashMap::new();
    let mut field_order = Vec::new();
    let mut field_visibility = HashMap::new();
    let mut methods: HashMap<String, Vec<Callable>> = HashMap::new();
    let mut properties = HashMap::new();
    let mut constructor = None;
    let mut constructor_visibility = None;
    let mut names = HashSet::new();
    for member in &declaration.members {
        match member {
            Member::Field(field) => {
                validate_member_visibility(field.visibility, field.span, kind, diagnostics);
                if kind == TypeKind::Interface {
                    diagnostics.push(
                        Diagnostic::error("interfaces cannot declare instance fields", field.span)
                            .with_help("replace the field with a parameterless method contract"),
                    );
                }
                if declaration.is_static {
                    diagnostics.push(
                        Diagnostic::error("a static class cannot declare fields", field.span)
                            .with_help("keep state outside the static utility class"),
                    );
                }
                if methods.contains_key(&field.name) || !names.insert(field.name.as_str()) {
                    duplicate_member(&field.name, field.span, diagnostics);
                }
                let type_ = resolve_type(&field.type_ref, context, diagnostics);
                if type_ == Type::Void {
                    diagnostics.push(Diagnostic::error(
                        "fields cannot have type `void`",
                        field.type_ref.span,
                    ));
                }
                fields.insert(field.name.clone(), type_);
                field_order.push(field.name.clone());
                field_visibility.insert(field.name.clone(), field.visibility);
            }
            Member::Method(method) => {
                validate_member_visibility(method.visibility, method.span, kind, diagnostics);
                if kind == TypeKind::Interface && method.is_static {
                    diagnostics.push(Diagnostic::error(
                        "static interface methods are not implemented",
                        method.span,
                    ));
                }
                if method.is_async && method.body.is_none() {
                    diagnostics.push(
                        Diagnostic::error(
                            "an `async` function must have a body",
                            method.span,
                        )
                        .with_help(
                            "`async` is only valid on free functions or static methods with a block body",
                        ),
                    );
                }
                if kind == TypeKind::Interface && method.visibility != Visibility::Public {
                    diagnostics.push(
                        Diagnostic::error(
                            "required interface members are public by contract",
                            method.span,
                        )
                        .with_help("remove the modifier or use `public`"),
                    );
                }
                if method.constructor {
                    if kind != TypeKind::Class {
                        diagnostics.push(Diagnostic::error(
                            "constructors are valid only in classes",
                            method.span,
                        ));
                    }
                    if declaration.is_static {
                        diagnostics.push(
                            Diagnostic::error(
                                "a static class cannot declare a constructor",
                                method.span,
                            )
                            .with_help(
                                "remove the constructor; static classes are never instantiated",
                            ),
                        );
                    }
                    if constructor.is_some() {
                        diagnostics.push(Diagnostic::error(
                            "constructor overloads are not implemented",
                            method.span,
                        ));
                    }
                    constructor = Some(Callable {
                        signature: signature(method, context, diagnostics),
                        visibility: method.visibility,
                        is_static: false,
                        is_foreign: false,
                        key: callable_key(
                            &method.name,
                            method.span.start,
                            None,
                            Some(&declaration.name),
                        ),
                    });
                    constructor_visibility = Some(method.visibility);
                    continue;
                }
                if declaration.is_static && !method.is_static {
                    diagnostics.push(
                        Diagnostic::error("members of a static class must be static", method.span)
                            .with_help("add `static` to the method"),
                    );
                }
                if names.contains(method.name.as_str()) {
                    duplicate_member(&method.name, method.span, diagnostics);
                }
                let callable = Callable {
                    signature: signature(method, context, diagnostics),
                    visibility: method.visibility,
                    is_static: method.is_static,
                    is_foreign: false,
                    key: callable_key(
                        &method.name,
                        method.span.start,
                        None,
                        Some(&declaration.name),
                    ),
                };
                let overloads = methods.entry(method.name.clone()).or_default();
                if overloads
                    .iter()
                    .any(|existing| existing.signature.parameters == callable.signature.parameters)
                {
                    diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "duplicate overload `{}.{}` with the same parameter types",
                                declaration.name, method.name
                            ),
                            method.span,
                        )
                        .with_help("change the parameter types; return type alone cannot distinguish overloads"),
                    );
                }
                overloads.push(callable);
            }
            Member::Property(property) => {
                validate_member_visibility(property.visibility, property.span, kind, diagnostics);
                if kind != TypeKind::Class {
                    diagnostics.push(Diagnostic::error(
                        "properties are currently supported only in classes",
                        property.span,
                    ));
                }
                if declaration.is_static {
                    diagnostics.push(
                        Diagnostic::error(
                            "static-class properties are not implemented",
                            property.span,
                        )
                        .with_help("use a static method in this phase"),
                    );
                }
                if methods.contains_key(&property.name) || !names.insert(property.name.as_str()) {
                    duplicate_member(&property.name, property.span, diagnostics);
                }
                let type_ = resolve_type(&property.type_ref, context, diagnostics);
                if type_ == Type::Void {
                    diagnostics.push(Diagnostic::error(
                        "properties cannot have type `void`",
                        property.type_ref.span,
                    ));
                }
                if property.getter.is_none() && property.setter.is_none() {
                    diagnostics.push(Diagnostic::error(
                        "a property requires a getter or setter",
                        property.span,
                    ));
                }
                for accessor in property.getter.iter().chain(property.setter.iter()) {
                    if accessor.explicit_visibility
                        && visibility_rank(accessor.visibility)
                            > visibility_rank(property.visibility)
                    {
                        diagnostics.push(
                            Diagnostic::error(
                                "an accessor cannot be more visible than its property",
                                accessor.span,
                            )
                            .with_help("remove the accessor modifier or make it more restrictive"),
                        );
                    }
                }
                let getter = property.getter.as_ref().map(|accessor| Callable {
                    signature: Signature {
                        parameters: Vec::new(),
                        result: type_.clone(),
                    },
                    visibility: accessor.visibility,
                    is_static: false,
                    is_foreign: false,
                    key: callable_key(
                        &property.name,
                        property.span.start,
                        Some(AccessorKind::Get),
                        Some(&declaration.name),
                    ),
                });
                let setter = property.setter.as_ref().map(|accessor| Callable {
                    signature: Signature {
                        parameters: vec![type_.clone()],
                        result: Type::Void,
                    },
                    visibility: accessor.visibility,
                    is_static: false,
                    is_foreign: false,
                    key: callable_key(
                        &property.name,
                        property.span.start,
                        Some(AccessorKind::Set),
                        Some(&declaration.name),
                    ),
                });
                properties.insert(
                    property.name.clone(),
                    PropertyInfo {
                        type_,
                        getter,
                        setter,
                    },
                );
            }
        }
    }
    context.types.insert(
        declaration.name.clone(),
        TypeInfo {
            is_static: declaration.is_static,
            fields,
            field_order,
            field_visibility,
            methods,
            properties,
            constructor,
            constructor_visibility,
            implemented_interfaces: declaration
                .interfaces
                .iter()
                .filter(|interface| {
                    matches!(
                        resolve_type_readonly(interface, context),
                        Type::Interface(_)
                    )
                })
                .map(|interface| interface.name.clone())
                .collect(),
            kind: Some(kind),
            enum_cases: Vec::new(),
        },
    );
}

pub(super) fn validate_interface_implementations(
    module: &Module,
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &module.items {
        let Item::Class(class) = item else {
            continue;
        };
        let Some(class_info) = context.types.get(&class.name) else {
            continue;
        };
        let mut seen = HashSet::new();
        for interface_ref in &class.interfaces {
            if !seen.insert(interface_ref.name.as_str()) {
                diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "interface `{}` is implemented more than once",
                            interface_ref.name
                        ),
                        interface_ref.span,
                    )
                    .with_help("remove the duplicate interface from the class declaration"),
                );
                continue;
            }
            let Some(interface_info) = context.types.get(&interface_ref.name) else {
                diagnostics.push(
                    Diagnostic::error(
                        format!("unknown interface `{}`", interface_ref.name),
                        interface_ref.span,
                    )
                    .with_help("declare the interface or add a using for its namespace"),
                );
                continue;
            };
            if interface_info.kind != Some(TypeKind::Interface) {
                diagnostics.push(
                    Diagnostic::error(
                        format!("`{}` is not an interface", interface_ref.name),
                        interface_ref.span,
                    )
                    .with_help(
                        "only interfaces may appear after `:`; class inheritance is not supported",
                    ),
                );
                continue;
            }
            for (method_name, required_overloads) in &interface_info.methods {
                for required in required_overloads {
                    let Some(actual_overloads) = class_info.methods.get(method_name) else {
                        diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "class `{}` does not implement required method `{}.{method_name}`",
                                class.name, interface_ref.name
                            ),
                            class.span,
                        )
                        .with_help(format!(
                            "add a public method `{method_name}` with the exact interface signature"
                        )),
                    );
                        continue;
                    };
                    let actual = actual_overloads
                        .iter()
                        .find(|actual| !actual.is_static && actual.signature == required.signature);
                    if actual.is_none() {
                        diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "method `{}.{method_name}` does not match interface `{}`",
                                    class.name, interface_ref.name
                                ),
                                class.span,
                            )
                            .with_help(
                                "parameter and return types must match the interface exactly",
                            ),
                        );
                    }
                    if actual.is_some_and(|actual| actual.visibility != Visibility::Public) {
                        diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "method `{}.{method_name}` must be public to implement `{}`",
                                    class.name, interface_ref.name
                                ),
                                class.span,
                            )
                            .with_help("mark the implementing method `public`"),
                        );
                    }
                }
            }
        }
    }
}

pub(super) fn validate_struct_cycles(
    module: &Module,
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
) {
    fn visit<'a>(
        name: &'a str,
        context: &'a Context,
        visiting: &mut Vec<&'a str>,
        complete: &mut HashSet<&'a str>,
    ) -> Option<Vec<&'a str>> {
        if let Some(index) = visiting.iter().position(|candidate| *candidate == name) {
            let mut cycle = visiting[index..].to_vec();
            cycle.push(name);
            return Some(cycle);
        }
        if complete.contains(name) {
            return None;
        }
        let info = context.types.get(name)?;
        if !matches!(info.kind, Some(TypeKind::Struct | TypeKind::Enum)) {
            return None;
        }
        visiting.push(name);
        let fields = info.fields.values().chain(
            info.enum_cases
                .iter()
                .flat_map(|case| case.fields.iter().map(|(_, type_)| type_)),
        );
        for field in fields {
            if let Type::User(next) | Type::Enum(next) = field
                && context.types.get(next).is_some_and(|next| {
                    matches!(next.kind, Some(TypeKind::Struct | TypeKind::Enum))
                })
                && let Some(cycle) = visit(next, context, visiting, complete)
            {
                return Some(cycle);
            }
        }
        visiting.pop();
        complete.insert(name);
        None
    }

    let mut complete = HashSet::new();
    for item in &module.items {
        let (name, span, label) = match item {
            Item::Struct(declaration) => (
                &declaration.name,
                declaration.span,
                "recursive struct layout",
            ),
            Item::Enum(declaration) => {
                (&declaration.name, declaration.span, "recursive enum layout")
            }
            _ => continue,
        };
        if let Some(cycle) = visit(name, context, &mut Vec::new(), &mut complete) {
            diagnostics.push(
                Diagnostic::error(
                    format!("{label}: {}", cycle.join(" -> ")),
                    span,
                )
                .with_help(
                    "break the cycle with a class, interface, or array reference; structs and enums are stored by value",
                ),
            );
        }
    }
}

fn signature(
    function: &FunctionDeclaration,
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
) -> Signature {
    let mut names = HashSet::new();
    let parameters = function
        .parameters
        .iter()
        .map(|parameter| {
            if !names.insert(parameter.name.as_str()) {
                diagnostics.push(
                    Diagnostic::error(
                        format!("duplicate parameter `{}`", parameter.name),
                        parameter.span,
                    )
                    .with_help("rename one of the parameters"),
                );
            }
            let type_ = resolve_type(&parameter.type_ref, context, diagnostics);
            if type_ == Type::Void {
                diagnostics.push(Diagnostic::error(
                    "parameters cannot have type `void`",
                    parameter.type_ref.span,
                ));
            }
            type_
        })
        .collect();
    Signature {
        parameters,
        result: resolve_type(&function.return_type, context, diagnostics),
    }
}

fn validate_foreign_declaration(
    function: &FunctionDeclaration,
    signature: &Signature,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !function.is_foreign {
        return;
    }
    if function.is_async {
        diagnostics.push(
            Diagnostic::error("foreign functions cannot be `async`", function.span)
                .with_help("wrap the foreign call in an ordinary safe function if needed"),
        );
    }
    let supported = |type_: &Type, allow_void: bool| {
        (allow_void && *type_ == Type::Void)
            || matches!(
                type_,
                Type::Bool
                    | Type::SByte
                    | Type::Byte
                    | Type::Short
                    | Type::UShort
                    | Type::Char
                    | Type::Int
                    | Type::UInt
                    | Type::Long
                    | Type::ULong
                    | Type::Float
                    | Type::Double
            )
    };
    if !supported(&signature.result, true) {
        diagnostics.push(
            Diagnostic::error(
                format!(
                    "foreign function result type `{}` is not supported",
                    signature.result.display()
                ),
                function.return_type.span,
            )
            .with_help("use `void` or an ABI-safe scalar type"),
        );
    }
    for (parameter, type_) in function.parameters.iter().zip(&signature.parameters) {
        if !supported(type_, false) {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "foreign parameter type `{}` is not supported",
                        type_.display()
                    ),
                    parameter.type_ref.span,
                )
                .with_help(
                    "use an ABI-safe scalar type; references and aggregates cannot cross FFI",
                ),
            );
        }
    }
}

pub(super) fn validate_module_variables(
    module: &Module,
    context: &mut Context,
    diagnostics: &mut Vec<Diagnostic>,
    model: &mut Model,
) {
    for item in &module.items {
        let Item::Variable(variable) = item else {
            continue;
        };
        let (binding, mut variable_diagnostics) = {
            let mut analyzer = Analyzer::new(
                context,
                Type::Void,
                HashMap::new(),
                HashMap::new(),
                None,
                false,
                false,
                &HashSet::new(),
                format!("#global:{}@{}", variable.name, variable.span.start),
                model,
            );
            let binding = analyzer.variable_binding(variable);
            (binding, analyzer.diagnostics)
        };
        if let Some(binding) = binding {
            context.globals.insert(variable.name.clone(), binding);
        }
        diagnostics.append(&mut variable_diagnostics);
    }
}

pub(super) fn validate_bodies(
    module: &Module,
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
    model: &mut Model,
) {
    for item in &module.items {
        match item {
            Item::Function(function) => {
                validate_function(function, context, None, diagnostics, model);
            }
            Item::Class(declaration) => {
                let initialized_fields = declaration
                    .members
                    .iter()
                    .filter_map(|member| match member {
                        Member::Field(field) if field.initializer.is_some() => {
                            Some(field.name.clone())
                        }
                        _ => None,
                    })
                    .collect::<HashSet<_>>();
                for member in &declaration.members {
                    match member {
                        Member::Field(field) => {
                            validate_field_initializer(
                                field,
                                &declaration.name,
                                context,
                                diagnostics,
                                model,
                            );
                        }
                        Member::Method(method) => validate_function_with_initialized_fields(
                            method,
                            context,
                            Some(declaration),
                            diagnostics,
                            model,
                            &initialized_fields,
                        ),
                        Member::Property(property) => {
                            validate_property(property, declaration, context, diagnostics, model);
                        }
                    }
                }
            }
            Item::Struct(declaration) => {
                for member in &declaration.members {
                    if let Member::Field(field) = member {
                        validate_field_initializer(
                            field,
                            &declaration.name,
                            context,
                            diagnostics,
                            model,
                        );
                    }
                    if let Member::Method(method) = member {
                        validate_function(method, context, Some(declaration), diagnostics, model);
                    }
                    if let Member::Property(property) = member {
                        diagnostics.push(Diagnostic::error(
                            "properties are currently supported only in classes",
                            property.span,
                        ));
                    }
                }
            }
            Item::Interface(_) | Item::Enum(_) | Item::Variable(_) => {}
        }
    }
}

fn validate_field_initializer(
    field: &Field,
    owner: &str,
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
    model: &mut Model,
) {
    let Some(initializer) = &field.initializer else {
        return;
    };
    let mut analyzer = Analyzer::new(
        context,
        Type::Void,
        HashMap::new(),
        HashMap::new(),
        None,
        false,
        false,
        &HashSet::new(),
        crate::semantic::field_context(owner, &field.name, field.span.start),
        model,
    );
    let actual = analyzer.expression(initializer);
    let expected = resolve_type_readonly(&field.type_ref, context);
    analyzer.require_assignable_value(&expected, &actual, initializer);
    diagnostics.append(&mut analyzer.diagnostics);
}

fn validate_function(
    function: &FunctionDeclaration,
    context: &Context,
    owner: Option<&TypeDeclaration>,
    diagnostics: &mut Vec<Diagnostic>,
    model: &mut Model,
) {
    validate_function_with_initialized_fields(
        function,
        context,
        owner,
        diagnostics,
        model,
        &HashSet::new(),
    );
}

fn validate_function_with_initialized_fields(
    function: &FunctionDeclaration,
    context: &Context,
    owner: Option<&TypeDeclaration>,
    diagnostics: &mut Vec<Diagnostic>,
    model: &mut Model,
    initialized_fields: &HashSet<String>,
) {
    validate_test_function(function, owner, diagnostics);
    if function.is_async && function.body.is_none() {
        diagnostics.push(
            Diagnostic::error("an `async` function must have a body", function.span).with_help(
                "`async` is only valid on free functions or static methods with a block body",
            ),
        );
        return;
    }
    let Some(body) = &function.body else {
        return;
    };
    let declared_return = resolve_type_readonly(&function.return_type, context);
    let return_type = if function.is_async {
        validate_async_function(function, body, &declared_return, owner, diagnostics)
    } else {
        declared_return
    };
    let (mut fields, methods) = owner
        .and_then(|owner| context.types.get(&owner.name))
        .map_or_else(
            || (HashMap::new(), HashMap::new()),
            |info| (info.fields.clone(), info.methods.clone()),
        );
    if function.is_static {
        fields.clear();
    }
    let owner_name = owner.map(|owner| owner.name.clone());
    let mut analyzer = Analyzer::new(
        context,
        return_type.clone(),
        fields,
        methods,
        owner_name,
        function.constructor,
        !function.is_static && owner.is_some(),
        initialized_fields,
        crate::semantic::function_context(function, owner),
        model,
    );
    if function.is_async {
        analyzer.async_state = super::AsyncAnalysisState::BeforeAwait;
    }
    for parameter in &function.parameters {
        analyzer.declare(
            &parameter.name,
            Binding {
                type_: resolve_type_readonly(&parameter.type_ref, context),
                mutable: true,
                iteration_readonly: false,
                initialized: true,
                span: parameter.span,
                value: None,
            },
        );
    }
    let flow = analyzer.block(body, true);
    if function.constructor {
        for field in analyzer.field_names.clone() {
            if analyzer
                .binding(&field)
                .is_some_and(|binding| !binding.initialized)
            {
                analyzer.diagnostics.push(
                    Diagnostic::error(
                        format!("constructor does not initialize field `{field}`"),
                        function.span,
                    )
                    .with_help("assign the field on every constructor path"),
                );
            }
        }
    }
    if return_type != Type::Void && flow.can_continue {
        analyzer.diagnostics.push(
            Diagnostic::error(
                format!(
                    "function `{}` must return `{}`",
                    function.name,
                    return_type.display()
                ),
                function.return_type.span,
            )
            .with_help("add a return statement with a compatible value"),
        );
    }
    diagnostics.append(&mut analyzer.diagnostics);
}

fn validate_test_function(
    function: &FunctionDeclaration,
    owner: Option<&TypeDeclaration>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !function.is_test {
        return;
    }
    if owner.is_some() {
        diagnostics.push(
            Diagnostic::error(
                "test functions must be declared at namespace level",
                function.span,
            )
            .with_help("move the test to a source file outside the type declaration"),
        );
    }
    if function.visibility == Visibility::Public {
        diagnostics.push(
            Diagnostic::error("test functions cannot be public", function.span)
                .with_help("tests are package-owned runner metadata; remove `public`"),
        );
    }
    if function.is_static || function.is_async || function.is_foreign || function.constructor {
        diagnostics.push(
            Diagnostic::error(
                "a test function must be synchronous and ordinary",
                function.span,
            )
            .with_help("write `test void Name() { ... }`"),
        );
    }
    if !function.type_parameters.is_empty() {
        diagnostics.push(
            Diagnostic::error("test functions cannot be generic", function.span)
                .with_help("declare one concrete parameterless test"),
        );
    }
    if !function.parameters.is_empty() {
        diagnostics.push(
            Diagnostic::error("test functions cannot declare parameters", function.span)
                .with_help("write `test void Name() { ... }`"),
        );
    }
    if function.return_type.name != "void" {
        diagnostics.push(
            Diagnostic::error(
                "test functions must return `void`",
                function.return_type.span,
            )
            .with_help("write `test void Name() { ... }`"),
        );
    }
    if function.body.is_none() {
        diagnostics.push(
            Diagnostic::error("test functions must have a body", function.span)
                .with_help("write a block body for the test"),
        );
    }
}
/// Enforce the restricted first-version `async`/`await` surface and return the
/// concrete `T` that `return` statements are checked against (the inner type of
/// the declared `Task<T>`), rather than the declared `Task<T>` itself.
fn validate_async_function(
    function: &FunctionDeclaration,
    body: &aster_syntax::Block,
    declared_return: &Type,
    owner: Option<&TypeDeclaration>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    if let Some(parameter) = function.parameters.first() {
        diagnostics.push(
            Diagnostic::error(
                "an `async` function cannot declare parameters in this version",
                parameter.span,
            )
            .with_help("async functions currently take no parameters"),
        );
    }
    if !function.is_static && owner.is_some() {
        diagnostics.push(
            Diagnostic::error(
                "`async` instance methods are not supported in this version",
                function.span,
            )
            .with_help("mark the method `static`"),
        );
    }
    let effective_return = match declared_return {
        Type::Task(inner) => {
            if matches!(**inner, Type::Void) {
                diagnostics.push(
                    Diagnostic::error(
                        "`async Task<void>` is not supported in this version",
                        function.return_type.span,
                    )
                    .with_help("return a scalar `Task<T>`, for example `Task<int>`"),
                );
            } else if !super::calls::transferable(inner) {
                diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`async Task<{}>` requires a scalar result `T` in this version",
                            inner.display()
                        ),
                        function.return_type.span,
                    )
                    .with_help("async supports only scalar results: bool, char, integers, floats"),
                );
            }
            (**inner).clone()
        }
        other => {
            diagnostics.push(
                Diagnostic::error(
                    "an `async` function must return `Task<T>`",
                    function.return_type.span,
                )
                .with_help("change the return type to `Task<T>` with a scalar `T`"),
            );
            other.clone()
        }
    };
    validate_async_body(body, diagnostics);
    effective_return
}

/// Structural checks for an async body: linear control flow, exactly one
/// `await`, and only the direct `await Task.Run(...)` form. The reference-local
/// rule runs later, inside the analyzer, where concrete local types are known.
fn validate_async_body(body: &aster_syntax::Block, diagnostics: &mut Vec<Diagnostic>) {
    for statement in &body.statements {
        validate_async_statement(statement, diagnostics);
    }
    let mut operands = Vec::new();
    for statement in &body.statements {
        collect_statement_awaits(statement, &mut operands);
    }
    match operands.as_slice() {
        [operand] => {
            let direct = matches!(
                &operand.kind,
                ExpressionKind::Call { callee, .. } if super::calls::is_task_run_callee(callee)
            );
            if !direct {
                diagnostics.push(
                    Diagnostic::error(
                        "`await` must directly await `Task.Run(...)` in this version",
                        operand.span,
                    )
                    .with_help("write `await Task.Run(Function)`"),
                );
            }
        }
        [] => diagnostics.push(
            Diagnostic::error(
                "an `async` function must contain exactly one `await` in this version",
                body.span,
            )
            .with_help("await one `Task.Run(...)`"),
        ),
        [_, second, ..] => diagnostics.push(
            Diagnostic::error(
                "an `async` function may contain only one `await` in this version",
                second.span,
            )
            .with_help("reduce the body to a single `await`"),
        ),
    }
    // The conservative "no reference local before the await" rule needs the
    // concrete (possibly inferred) type of each local, so it runs inside the
    // analyzer (see `Analyzer::statement`), not this pre-analysis AST pass.

    // `Parallel` anywhere in an async body (not only around the await) is
    // rejected structurally here. `Task<T>.Wait()` needs the resolved receiver
    // type, so `Analyzer::call` rejects that intrinsic precisely instead of
    // treating every unrelated method named `Wait` as concurrency.
    let mut calls = Vec::new();
    for statement in &body.statements {
        collect_statement_calls(statement, &mut calls);
    }
    for call in calls {
        let ExpressionKind::Call { callee, .. } = &call.kind else {
            continue;
        };
        if super::calls::is_parallel_for_callee(callee)
            || super::calls::is_parallel_for_each_callee(callee)
        {
            diagnostics.push(
                Diagnostic::error(
                    "`Parallel` is not supported inside an `async` function in this version",
                    call.span,
                )
                .with_help("call `Parallel.For`/`Parallel.ForEach` outside async functions"),
            );
        }
    }
}

fn validate_async_statement(statement: &Statement, diagnostics: &mut Vec<Diagnostic>) {
    match statement {
        Statement::If { span, .. }
        | Statement::While { span, .. }
        | Statement::For { span, .. }
        | Statement::ForEach { span, .. }
        | Statement::Switch { span, .. }
        | Statement::Break(span)
        | Statement::Continue(span) => diagnostics.push(
            Diagnostic::error(
                "an `async` function must have linear control flow in this version",
                *span,
            )
            .with_help("remove `if`, `switch`, and loops from async bodies"),
        ),
        Statement::Unsafe { body, .. } => {
            for statement in &body.statements {
                validate_async_statement(statement, diagnostics);
            }
        }
        Statement::Variable(_) | Statement::Return { .. } | Statement::Expression(_) => {}
    }
}

/// Every `Call` expression node reachable from `statement`, for the
/// structural `Wait`/`Parallel` rejection above. Mirrors
/// `collect_statement_awaits`/`collect_expression_awaits` but collects every
/// call instead of only `await` operands, and also descends into
/// non-linear control flow (already flagged separately) so a `Wait`/`Parallel`
/// hidden inside one is still reported.
pub(super) fn collect_statement_calls<'a>(statement: &'a Statement, out: &mut Vec<&'a Expression>) {
    match statement {
        Statement::Variable(variable) => {
            if let Some(initializer) = &variable.initializer {
                collect_expression_calls(initializer, out);
            }
        }
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                collect_expression_calls(value, out);
            }
        }
        Statement::Expression(expression) => collect_expression_calls(expression, out),
        Statement::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            collect_expression_calls(condition, out);
            for statement in &then_block.statements {
                collect_statement_calls(statement, out);
            }
            if let Some(else_block) = else_block {
                for statement in &else_block.statements {
                    collect_statement_calls(statement, out);
                }
            }
        }
        Statement::While {
            condition, body, ..
        } => {
            collect_expression_calls(condition, out);
            for statement in &body.statements {
                collect_statement_calls(statement, out);
            }
        }
        Statement::For {
            initializer,
            condition,
            update,
            body,
            ..
        } => {
            if let Some(initializer) = initializer {
                collect_statement_calls(initializer, out);
            }
            if let Some(condition) = condition {
                collect_expression_calls(condition, out);
            }
            if let Some(update) = update {
                collect_expression_calls(update, out);
            }
            for statement in &body.statements {
                collect_statement_calls(statement, out);
            }
        }
        Statement::ForEach {
            collection, body, ..
        } => {
            collect_expression_calls(collection, out);
            for statement in &body.statements {
                collect_statement_calls(statement, out);
            }
        }
        Statement::Switch {
            value,
            cases,
            default,
            ..
        } => {
            collect_expression_calls(value, out);
            for case in cases {
                for statement in &case.body.statements {
                    collect_statement_calls(statement, out);
                }
            }
            if let Some(default) = default {
                for statement in &default.statements {
                    collect_statement_calls(statement, out);
                }
            }
        }
        Statement::Unsafe { body, .. } => {
            for statement in &body.statements {
                collect_statement_calls(statement, out);
            }
        }
        Statement::Break(_) | Statement::Continue(_) => {}
    }
}

fn collect_expression_calls<'a>(expression: &'a Expression, out: &mut Vec<&'a Expression>) {
    if matches!(expression.kind, ExpressionKind::Call { .. }) {
        out.push(expression);
    }
    match &expression.kind {
        ExpressionKind::Literal(_) | ExpressionKind::Name(_) | ExpressionKind::This => {}
        ExpressionKind::StructLiteral { fields, .. } => {
            for field in fields {
                collect_expression_calls(&field.value, out);
            }
        }
        ExpressionKind::ArrayLiteral(elements) => {
            for element in elements {
                collect_expression_calls(element, out);
            }
        }
        ExpressionKind::NewArray { length, .. } => collect_expression_calls(length, out),
        ExpressionKind::NewObject { arguments, .. } => {
            for argument in arguments {
                collect_expression_calls(argument, out);
            }
        }
        ExpressionKind::Index { array, index } => {
            collect_expression_calls(array, out);
            collect_expression_calls(index, out);
        }
        ExpressionKind::Member { object, .. } => collect_expression_calls(object, out),
        ExpressionKind::Call {
            callee, arguments, ..
        } => {
            collect_expression_calls(callee, out);
            for argument in arguments {
                collect_expression_calls(argument, out);
            }
        }
        ExpressionKind::Unary { operand, .. }
        | ExpressionKind::IncrementDecrement { operand, .. }
        | ExpressionKind::Try { operand }
        | ExpressionKind::Await { operand }
        | ExpressionKind::Cast { operand, .. } => collect_expression_calls(operand, out),
        ExpressionKind::Conditional {
            condition,
            when_true,
            when_false,
        } => {
            collect_expression_calls(condition, out);
            collect_expression_calls(when_true, out);
            collect_expression_calls(when_false, out);
        }
        ExpressionKind::Switch {
            value,
            cases,
            default,
        } => {
            collect_expression_calls(value, out);
            for case in cases {
                collect_expression_calls(&case.value, out);
            }
            if let Some(default) = default {
                collect_expression_calls(default, out);
            }
        }
        ExpressionKind::Binary { left, right, .. } => {
            collect_expression_calls(left, out);
            collect_expression_calls(right, out);
        }
        ExpressionKind::Assignment { target, value, .. } => {
            collect_expression_calls(target, out);
            collect_expression_calls(value, out);
        }
        ExpressionKind::InterpolatedString { parts } => {
            for part in parts {
                if let InterpolatedPart::Expression(expression) = part {
                    collect_expression_calls(expression, out);
                }
            }
        }
    }
}

pub(super) fn is_reference_type(type_: &Type) -> bool {
    matches!(
        type_,
        Type::String | Type::Array(_) | Type::Class(_) | Type::Interface(_) | Type::Task(_)
    )
}

fn collect_statement_awaits<'a>(statement: &'a Statement, out: &mut Vec<&'a Expression>) {
    match statement {
        Statement::Variable(variable) => {
            if let Some(initializer) = &variable.initializer {
                collect_expression_awaits(initializer, out);
            }
        }
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                collect_expression_awaits(value, out);
            }
        }
        Statement::Expression(expression) => collect_expression_awaits(expression, out),
        Statement::If { .. }
        | Statement::While { .. }
        | Statement::For { .. }
        | Statement::ForEach { .. }
        | Statement::Switch { .. }
        | Statement::Break(_)
        | Statement::Continue(_) => {}
        Statement::Unsafe { body, .. } => {
            for statement in &body.statements {
                collect_statement_awaits(statement, out);
            }
        }
    }
}

fn collect_expression_awaits<'a>(expression: &'a Expression, out: &mut Vec<&'a Expression>) {
    match &expression.kind {
        ExpressionKind::Await { operand } => {
            out.push(operand);
            collect_expression_awaits(operand, out);
        }
        ExpressionKind::Unary { operand, .. }
        | ExpressionKind::IncrementDecrement { operand, .. }
        | ExpressionKind::Try { operand }
        | ExpressionKind::Cast { operand, .. } => collect_expression_awaits(operand, out),
        ExpressionKind::Binary { left, right, .. } => {
            collect_expression_awaits(left, out);
            collect_expression_awaits(right, out);
        }
        ExpressionKind::Assignment { target, value, .. } => {
            collect_expression_awaits(target, out);
            collect_expression_awaits(value, out);
        }
        ExpressionKind::Conditional {
            condition,
            when_true,
            when_false,
        } => {
            collect_expression_awaits(condition, out);
            collect_expression_awaits(when_true, out);
            collect_expression_awaits(when_false, out);
        }
        ExpressionKind::Switch {
            value,
            cases,
            default,
        } => {
            collect_expression_awaits(value, out);
            for case in cases {
                collect_expression_awaits(&case.value, out);
            }
            if let Some(default) = default {
                collect_expression_awaits(default, out);
            }
        }
        ExpressionKind::Call {
            callee, arguments, ..
        } => {
            collect_expression_awaits(callee, out);
            for argument in arguments {
                collect_expression_awaits(argument, out);
            }
        }
        ExpressionKind::Member { object, .. } => collect_expression_awaits(object, out),
        ExpressionKind::Index { array, index } => {
            collect_expression_awaits(array, out);
            collect_expression_awaits(index, out);
        }
        ExpressionKind::NewArray { length, .. } => collect_expression_awaits(length, out),
        ExpressionKind::NewObject { arguments, .. } => {
            for argument in arguments {
                collect_expression_awaits(argument, out);
            }
        }
        ExpressionKind::ArrayLiteral(elements) => {
            for element in elements {
                collect_expression_awaits(element, out);
            }
        }
        ExpressionKind::StructLiteral { fields, .. } => {
            for field in fields {
                collect_expression_awaits(&field.value, out);
            }
        }
        ExpressionKind::InterpolatedString { parts } => {
            for part in parts {
                if let InterpolatedPart::Expression(expression) = part {
                    collect_expression_awaits(expression, out);
                }
            }
        }
        ExpressionKind::Literal(_) | ExpressionKind::Name(_) | ExpressionKind::This => {}
    }
}

fn validate_property(
    property: &Property,
    owner: &TypeDeclaration,
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
    model: &mut Model,
) {
    if let Some(getter) = &property.getter {
        let function = FunctionDeclaration {
            constructor: false,
            is_test: false,
            is_static: false,
            is_async: false,
            is_foreign: false,
            type_parameters: Vec::new(),
            visibility: getter.visibility,
            return_type: property.type_ref.clone(),
            name: format!("get_{}", property.name),
            parameters: Vec::new(),
            body: Some(getter.body.clone()),
            span: getter.span,
        };
        validate_function(&function, context, Some(owner), diagnostics, model);
    }
    if let Some(setter) = &property.setter {
        let function = FunctionDeclaration {
            constructor: false,
            is_test: false,
            is_static: false,
            is_async: false,
            is_foreign: false,
            type_parameters: Vec::new(),
            visibility: setter.visibility,
            return_type: TypeRef::new("void", property.type_ref.span),
            name: format!("set_{}", property.name),
            parameters: vec![aster_syntax::Parameter {
                type_ref: property.type_ref.clone(),
                name: "value".to_owned(),
                span: setter.span,
            }],
            body: Some(setter.body.clone()),
            span: setter.span,
        };
        validate_function(&function, context, Some(owner), diagnostics, model);
    }
}

fn validate_module_visibility(
    visibility: Visibility,
    span: Span,
    declaration: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let span = explicit_visibility_span(visibility, span);
    match visibility {
        Visibility::Private => diagnostics.push(
            Diagnostic::error(
                format!("`private` is not valid on a namespace-level {declaration}"),
                span,
            )
            .with_help("use `internal`, `public`, or omit the modifier for internal visibility"),
        ),
        Visibility::Protected => diagnostics.push(
            Diagnostic::error(
                format!("`protected` is not valid on a namespace-level {declaration}"),
                span,
            )
            .with_help(
                "use `internal` or `public`; protected access requires a future extension model",
            ),
        ),
        Visibility::Public | Visibility::Internal => {}
    }
}

fn validate_member_visibility(
    visibility: Visibility,
    span: Span,
    owner: TypeKind,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if visibility == Visibility::Protected {
        let span = explicit_visibility_span(visibility, span);
        let message = if owner == TypeKind::Class {
            "`protected` members depend on a future inheritance or extension model"
        } else {
            "`protected` is only valid for class members"
        };
        diagnostics.push(
            Diagnostic::error(message, span)
                .with_help("use `private`, `internal`, or `public` in this frontend phase"),
        );
    }
}

const fn visibility_rank(visibility: Visibility) -> u8 {
    match visibility {
        Visibility::Private => 0,
        Visibility::Protected => 1,
        Visibility::Internal => 2,
        Visibility::Public => 3,
    }
}

fn explicit_visibility_span(visibility: Visibility, declaration_span: Span) -> Span {
    let width = match visibility {
        Visibility::Public | Visibility::Private => 6,
        Visibility::Internal | Visibility::Protected => 8,
    };
    Span::new(declaration_span.start, declaration_span.start + width)
}

fn duplicate_member(name: &str, span: Span, diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.push(
        Diagnostic::error(format!("duplicate member `{name}`"), span)
            .with_help("rename or remove one of the members"),
    );
}
