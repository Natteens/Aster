use std::collections::{HashMap, HashSet};

use aster_diagnostics::{Diagnostic, Span};

use crate::constexpr::{ConstError, ConstValue, evaluate, integer_value};
use crate::primitives::{
    self, IntegerFit, Primitive, UnsignedFit, classify_integer, classify_unsigned, fits_long,
    fits_ulong,
};
use aster_syntax::{
    AssignmentOperator, BinaryOperator, Block, Expression, ExpressionKind, Field,
    FunctionDeclaration, IncrementOperator, Item, Literal, Member, Module, Property, Statement,
    TypeDeclaration, TypeRef, UnaryOperator, VariableDeclaration, VariableKind, Visibility,
};

use super::{
    AccessorKind, CallableKey, Dispatch, Model, ResolvedCall, ResolvedEnumCase,
    ResolvedPropagation, ResolvedPropertyAssignment, callable_key,
};
use crate::type_names::TypeName;

pub(super) fn validate(module: &Module, diagnostics: &mut Vec<Diagnostic>, model: &mut Model) {
    let mut context = Context::default();
    collect_type_names(module, &mut context);
    collect_declarations(module, &mut context, diagnostics);
    discover_official_types(&mut context);
    validate_interface_implementations(module, &context, diagnostics);
    validate_struct_cycles(module, &context, diagnostics);
    validate_module_variables(module, &mut context, diagnostics, model);
    validate_bodies(module, &context, diagnostics, model);
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Type {
    Void,
    Bool,
    SByte,
    Byte,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    Float,
    Double,
    Decimal,
    Char,
    String,
    User(String),
    Class(String),
    Interface(String),
    Enum(String),
    Array(Box<Type>),
    Unknown,
}

impl Type {
    fn display(&self) -> String {
        match self {
            Self::Void => "void".to_owned(),
            Self::User(name) | Self::Class(name) | Self::Interface(name) | Self::Enum(name) => {
                name.clone()
            }
            Self::Array(element) => format!("{}[]", element.display()),
            Self::Unknown => "<unknown>".to_owned(),
            _ => self
                .primitive()
                .expect("non-void, non-user type")
                .name()
                .to_owned(),
        }
    }

    /// The primitive behind this type, when it has one. All numeric rules
    /// (conversions, promotion, ranges) live in `crate::primitives`.
    fn primitive(&self) -> Option<Primitive> {
        Some(match self {
            Self::Bool => Primitive::Bool,
            Self::Char => Primitive::Char,
            Self::SByte => Primitive::SByte,
            Self::Byte => Primitive::Byte,
            Self::Short => Primitive::Short,
            Self::UShort => Primitive::UShort,
            Self::Int => Primitive::Int,
            Self::UInt => Primitive::UInt,
            Self::Long => Primitive::Long,
            Self::ULong => Primitive::ULong,
            Self::Float => Primitive::Float,
            Self::Double => Primitive::Double,
            Self::Decimal => Primitive::Decimal,
            Self::String => Primitive::String,
            Self::Void
            | Self::User(_)
            | Self::Class(_)
            | Self::Interface(_)
            | Self::Enum(_)
            | Self::Array(_)
            | Self::Unknown => {
                return None;
            }
        })
    }

    fn from_primitive(primitive: Primitive) -> Self {
        match primitive {
            Primitive::Bool => Self::Bool,
            Primitive::Char => Self::Char,
            Primitive::SByte => Self::SByte,
            Primitive::Byte => Self::Byte,
            Primitive::Short => Self::Short,
            Primitive::UShort => Self::UShort,
            Primitive::Int => Self::Int,
            Primitive::UInt => Self::UInt,
            Primitive::Long => Self::Long,
            Primitive::ULong => Self::ULong,
            Primitive::Float => Self::Float,
            Primitive::Double => Self::Double,
            Primitive::Decimal => Self::Decimal,
            Primitive::String => Self::String,
        }
    }

    fn is_numeric(&self) -> bool {
        self.primitive().is_some_and(Primitive::is_numeric)
    }

    fn is_float(&self) -> bool {
        self.primitive().is_some_and(Primitive::is_float)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Signature {
    parameters: Vec<Type>,
    result: Type,
}

#[derive(Clone, Debug)]
struct Callable {
    signature: Signature,
    visibility: Visibility,
    is_static: bool,
    key: CallableKey,
}

#[derive(Clone, Debug)]
struct PropertyInfo {
    type_: Type,
    getter: Option<Callable>,
    setter: Option<Callable>,
}

#[derive(Clone, Debug, Default)]
struct TypeInfo {
    is_static: bool,
    fields: HashMap<String, Type>,
    field_order: Vec<String>,
    field_visibility: HashMap<String, Visibility>,
    methods: HashMap<String, Vec<Callable>>,
    properties: HashMap<String, PropertyInfo>,
    constructor: Option<Callable>,
    constructor_visibility: Option<Visibility>,
    implemented_interfaces: Vec<String>,
    kind: Option<TypeKind>,
    enum_cases: Vec<EnumCaseInfo>,
}

#[derive(Clone, Debug)]
struct EnumCaseInfo {
    name: String,
    fields: Vec<(String, Type)>,
}

/// Resolved `Ok`/`Error` positions and payload types of an official `Result`.
struct ResultCases {
    ok_index: usize,
    error_index: usize,
    success: Type,
    error: Type,
}

#[derive(Clone, Debug, Default)]
struct Context {
    types: HashMap<String, TypeInfo>,
    functions: HashMap<String, Vec<Callable>>,
    globals: HashMap<String, Binding>,
    /// Nominal identity (linked base name) of the official `aster.core.Result`
    /// and `Option`, discovered once from the loaded declarations. `None` when
    /// the core standard library is absent, in which case no type is official.
    official_result: Option<String>,
    official_option: Option<String>,
}

#[derive(Clone, Debug)]
struct Binding {
    type_: Type,
    mutable: bool,
    initialized: bool,
    span: Span,
    /// The evaluated value of a `const` binding; `None` for variables.
    value: Option<ConstValue>,
}

fn collect_type_names(module: &Module, context: &mut Context) {
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
fn discover_official_types(context: &mut Context) {
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
fn collect_declarations(module: &Module, context: &mut Context, diagnostics: &mut Vec<Diagnostic>) {
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
                    key: callable_key(&function.name, function.span.start, None, None),
                };
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

fn validate_interface_implementations(
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

fn validate_struct_cycles(module: &Module, context: &Context, diagnostics: &mut Vec<Diagnostic>) {
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

fn validate_module_variables(
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

fn validate_bodies(
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
    let Some(body) = &function.body else {
        return;
    };
    let return_type = resolve_type_readonly(&function.return_type, context);
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
    for parameter in &function.parameters {
        analyzer.declare(
            &parameter.name,
            Binding {
                type_: resolve_type_readonly(&parameter.type_ref, context),
                mutable: true,
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
            is_static: false,
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
            is_static: false,
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

struct Analyzer<'a> {
    context: &'a Context,
    scopes: Vec<HashMap<String, Binding>>,
    return_type: Type,
    methods: HashMap<String, Vec<Callable>>,
    owner: Option<String>,
    constructor: bool,
    instance_context: bool,
    model: &'a mut Model,
    field_names: HashSet<String>,
    diagnostics: Vec<Diagnostic>,
    loop_depth: usize,
    model_context: String,
}

#[derive(Clone, Copy)]
struct Flow {
    can_continue: bool,
}

impl Flow {
    const CONTINUE: Self = Self { can_continue: true };
    const TERMINATE: Self = Self {
        can_continue: false,
    };
}

impl<'a> Analyzer<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        context: &'a Context,
        return_type: Type,
        fields: HashMap<String, Type>,
        methods: HashMap<String, Vec<Callable>>,
        owner: Option<String>,
        constructor: bool,
        instance_context: bool,
        initialized_fields: &HashSet<String>,
        model_context: String,
        model: &'a mut Model,
    ) -> Self {
        let mut outer = context.globals.clone();
        let field_names = fields.keys().cloned().collect();
        outer.extend(fields.into_iter().map(|(name, type_)| {
            let initialized = initialized_fields.contains(&name)
                || !constructor
                || zero_initializable(&type_, context, &mut HashSet::new());
            (
                name,
                Binding {
                    type_,
                    mutable: true,
                    initialized,
                    span: Span::default(),
                    value: None,
                },
            )
        }));
        Self {
            context,
            scopes: vec![outer, HashMap::new()],
            return_type,
            methods,
            owner,
            constructor,
            instance_context,
            model,
            field_names,
            diagnostics: Vec::new(),
            loop_depth: 0,
            model_context,
        }
    }

    fn model_key(&self, span: Span) -> crate::semantic::ModelNodeKey {
        crate::semantic::ModelNodeKey {
            context: self.model_context.clone(),
            span,
        }
    }

    fn block(&mut self, block: &Block, create_scope: bool) -> Flow {
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
                        let actual = self.expression(value);
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
                                    "constructor returns before field `{field}` is initialized"
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
            Statement::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                let condition_type = self.expression(condition);
                self.require_bool_condition("if", &condition_type, condition.span);
                let before = self.scopes.clone();
                let then_flow = self.block(then_block, true);
                let then_scopes = self.scopes.clone();
                self.scopes.clone_from(&before);
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
                let condition_type = self.expression(condition);
                self.require_bool_condition("while", &condition_type, condition.span);
                self.loop_depth += 1;
                let before = self.scopes.clone();
                self.block(body, true);
                self.scopes = before;
                self.loop_depth -= 1;
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
                if let Some(condition) = condition {
                    let condition_type = self.expression(condition);
                    self.require_bool_condition("for", &condition_type, condition.span);
                }
                if let Some(update) = update {
                    self.expression(update);
                }
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
        let Type::Enum(enum_name) = selected else {
            if selected != Type::Unknown {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`switch` requires an enum value, found `{}`",
                            selected.display()
                        ),
                        value.span,
                    )
                    .with_help("select a declared enum value"),
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
        let mut covered = HashSet::new();
        let mut any_continues = false;
        for case in cases {
            if let Some(owner) = &case.enum_name
                && owner != &enum_name
            {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "case `{}` belongs to `{owner}`, not `{enum_name}`",
                        case.case_name
                    ),
                    case.span,
                ));
            }
            let Some((case_index, info)) = enum_cases
                .iter()
                .enumerate()
                .find(|(_, item)| item.name == case.case_name)
            else {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("enum `{enum_name}` has no case `{}`", case.case_name),
                        case.span,
                    )
                    .with_help("use one of the cases declared by the selected enum"),
                );
                self.block(&case.body, true);
                any_continues = true;
                continue;
            };
            if !covered.insert(case_index) {
                self.diagnostics.push(Diagnostic::error(
                    format!("duplicate switch case `{}`", case.case_name),
                    case.span,
                ));
            }
            if case.bindings.len() != info.fields.len() {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "case `{}` expects {} binding(s), found {}",
                            case.case_name,
                            info.fields.len(),
                            case.bindings.len()
                        ),
                        case.span,
                    )
                    .with_help("bind each payload value exactly once"),
                );
            }
            self.model.switch_cases.insert(
                self.model_key(case.span),
                ResolvedEnumCase {
                    enum_name: enum_name.clone(),
                    case_index,
                },
            );
            self.scopes.push(HashMap::new());
            for (binding, (_, type_)) in case.bindings.iter().zip(&info.fields) {
                self.declare(
                    binding,
                    Binding {
                        type_: type_.clone(),
                        mutable: true,
                        initialized: true,
                        span: case.span,
                        value: None,
                    },
                );
            }
            let flow = self.block(&case.body, false);
            self.scopes.pop();
            any_continues |= flow.can_continue;
        }
        if let Some(default) = default {
            if covered.len() == enum_cases.len() {
                self.diagnostics.push(
                    Diagnostic::warning("unreachable `default` case", default.span)
                        .with_help("remove `default`; every enum case is already covered"),
                );
            }
            any_continues |= self.block(default, true).can_continue;
        } else if covered.len() != enum_cases.len() {
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
            any_continues = true;
        }
        Flow {
            can_continue: any_continues,
        }
    }

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

    fn variable_binding(&mut self, variable: &VariableDeclaration) -> Option<Binding> {
        let initializer_type = variable
            .initializer
            .as_ref()
            .map(|value| self.expression(value));
        let (type_, mutable) = match &variable.kind {
            VariableKind::Explicit(type_ref) => {
                let expected = self.resolve_local_type(type_ref);
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
                let expected = self.resolve_local_type(type_ref);
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
                    initialized: true,
                    span: variable.span,
                    value,
                });
            }
        };
        Some(Binding {
            type_,
            mutable,
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

    fn declare(&mut self, name: &str, binding: Binding) {
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

    fn resolve_local_type(&mut self, type_ref: &TypeRef) -> Type {
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

    fn expression(&mut self, expression: &Expression) -> Type {
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
        }
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

    fn new_object(&mut self, type_name: &str, arguments: &[Expression], span: Span) -> Type {
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

    fn name(&mut self, name: &str, span: Span) -> Type {
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
    fn member(&mut self, object: &Expression, name: &str, span: Span) -> Type {
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
    fn call(&mut self, callee: &Expression, arguments: &[Expression], span: Span) -> Type {
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
    fn require_assignable_value(&mut self, expected: &Type, actual: &Type, value: &Expression) {
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

    fn require_assignable(&mut self, expected: &Type, actual: &Type, span: Span) {
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

    fn compatible(&self, expected: &Type, actual: &Type) -> bool {
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

    fn binding(&self, name: &str) -> Option<&Binding> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    fn binding_mut(&mut self, name: &str) -> Option<&mut Binding> {
        self.scopes
            .iter_mut()
            .rev()
            .find_map(|scope| scope.get_mut(name))
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

fn resolve_type(type_ref: &TypeRef, context: &Context, diagnostics: &mut Vec<Diagnostic>) -> Type {
    let type_ = resolve_type_readonly(type_ref, context);
    if type_ == Type::Unknown {
        if context
            .types
            .get(type_ref.name.as_str())
            .is_some_and(|info| info.is_static)
        {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "static class `{}` cannot be used as a value type",
                        type_ref.name
                    ),
                    type_ref.span,
                )
                .with_help("call its static methods through the class name"),
            );
        } else {
            diagnostics.push(
                Diagnostic::error(format!("unknown type `{}`", type_ref.name), type_ref.span)
                    .with_help("declare the type or use a known basic type"),
            );
        }
    }
    type_
}

fn resolve_type_readonly(type_ref: &TypeRef, context: &Context) -> Type {
    if let Some(element) = type_ref.name.strip_suffix("[]") {
        let element = resolve_type_readonly(&TypeRef::new(element, type_ref.span), context);
        return if matches!(element, Type::Void | Type::Unknown) {
            Type::Unknown
        } else {
            Type::Array(Box::new(element))
        };
    }
    if type_ref.name == "void" {
        return Type::Void;
    }
    if let Some(primitive) = primitives::from_name(&type_ref.name) {
        return Type::from_primitive(primitive);
    }
    let info = context.types.get(type_ref.name.as_str());
    if info.is_some_and(|info| info.is_static) {
        return Type::Unknown;
    }
    match info.and_then(|info| info.kind) {
        Some(TypeKind::Class) => Type::Class(type_ref.name.clone()),
        Some(TypeKind::Interface) => Type::Interface(type_ref.name.clone()),
        Some(TypeKind::Enum) => Type::Enum(type_ref.name.clone()),
        Some(_) => Type::User(type_ref.name.clone()),
        None => Type::Unknown,
    }
}

fn compatible(expected: &Type, actual: &Type) -> bool {
    if expected == actual {
        return true;
    }
    match (expected.primitive(), actual.primitive()) {
        (Some(expected), Some(actual)) => primitives::implicit_converts(actual, expected),
        _ => false,
    }
}

fn constant_integer(expression: &Expression) -> Option<i128> {
    match &expression.kind {
        ExpressionKind::Literal(Literal::Integer(value)) => value.parse().ok(),
        ExpressionKind::Unary {
            operator: UnaryOperator::Negate,
            operand,
        } => constant_integer(operand)?.checked_neg(),
        _ => None,
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

fn zero_initializable(type_: &Type, context: &Context, visiting: &mut HashSet<String>) -> bool {
    match type_ {
        Type::String
        | Type::Decimal
        | Type::Array(_)
        | Type::Class(_)
        | Type::Interface(_)
        | Type::Enum(_)
        | Type::Void
        | Type::Unknown => false,
        Type::User(name) => {
            if !visiting.insert(name.clone()) {
                return false;
            }
            let result = context.types.get(name).is_some_and(|info| {
                info.fields
                    .values()
                    .all(|field| zero_initializable(field, context, visiting))
            });
            visiting.remove(name);
            result
        }
        _ => true,
    }
}

/// The common type of two numeric operands, per the documented promotion table.
fn promoted_numeric(left: &Type, right: &Type) -> Option<Type> {
    let (left, right) = (left.primitive()?, right.primitive()?);
    primitives::promote(left, right).map(Type::from_primitive)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypeKind {
    Class,
    Struct,
    Interface,
    Enum,
}
