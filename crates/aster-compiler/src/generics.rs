use std::{collections::HashMap, fmt::Write};

use aster_diagnostics::Diagnostic;
use aster_syntax::{
    BinaryOperator, Block, EnumDeclaration, Expression, ExpressionKind, FunctionDeclaration, Item,
    Literal, Member, Module, Statement, SwitchCase, TypeDeclaration, TypeRef, VariableDeclaration,
    VariableKind,
    visit::{AstVisitorMut, walk_expression_mut, walk_switch_case_mut},
};

use crate::type_names::TypeName;

pub(crate) fn monomorphize(module: &mut Module) -> Vec<Diagnostic> {
    Monomorphizer::new(module).run(module)
}

struct Monomorphizer {
    templates: HashMap<String, FunctionDeclaration>,
    type_templates: HashMap<String, GenericTypeTemplate>,
    enum_templates: HashMap<String, EnumDeclaration>,
    returns: HashMap<String, String>,
    fields: HashMap<(String, String), String>,
    methods: HashMap<(String, String), String>,
    cache: HashMap<(String, Vec<String>), String>,
    active: Vec<(String, Vec<String>)>,
    generated: Vec<FunctionDeclaration>,
    type_cache: HashMap<(String, Vec<TypeName>), String>,
    type_active: Vec<(String, Vec<TypeName>)>,
    generated_types: Vec<Item>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone)]
enum GenericTypeTemplate {
    Class(TypeDeclaration),
    Struct(TypeDeclaration),
    Interface(TypeDeclaration),
}

impl GenericTypeTemplate {
    fn declaration(&self) -> &TypeDeclaration {
        match self {
            Self::Class(value) | Self::Struct(value) | Self::Interface(value) => value,
        }
    }

    fn into_item(self, declaration: TypeDeclaration) -> Item {
        match self {
            Self::Class(_) => Item::Class(declaration),
            Self::Struct(_) => Item::Struct(declaration),
            Self::Interface(_) => Item::Interface(declaration),
        }
    }
}

impl Monomorphizer {
    #[allow(clippy::too_many_lines)]
    fn new(module: &Module) -> Self {
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

    #[allow(clippy::too_many_lines)]
    fn run(mut self, module: &mut Module) -> Vec<Diagnostic> {
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
        module.items.retain(|item| {
            !matches!(item, Item::Function(function) if !function.type_parameters.is_empty())
                && !matches!(item, Item::Class(value) | Item::Struct(value) | Item::Interface(value) if !value.type_parameters.is_empty())
                && !matches!(item, Item::Enum(value) if !value.type_parameters.is_empty())
        });
        for item in &mut module.items {
            GenericTypeConcretizer::new(&mut self).visit_item_mut(item);
            match item {
                Item::Function(function) => self.function(function),
                Item::Class(declaration)
                | Item::Struct(declaration)
                | Item::Interface(declaration) => {
                    self.analyze_type_declaration(declaration);
                }
                Item::Enum(_) | Item::Variable(_) => {}
            }
        }
        module.items.append(&mut self.generated_types);
        module
            .items
            .extend(self.generated.drain(..).map(Item::Function));
        self.diagnostics
    }

    fn function(&mut self, function: &mut FunctionDeclaration) {
        let mut environment = function
            .parameters
            .iter()
            .map(|parameter| (parameter.name.clone(), parameter.type_ref.name.clone()))
            .collect();
        if let Some(body) = &mut function.body {
            self.block(body, &mut environment);
        }
    }

    fn analyze_type_declaration(&mut self, declaration: &mut TypeDeclaration) {
        for member in &declaration.members {
            match member {
                Member::Field(field) => {
                    self.fields.insert(
                        (declaration.name.clone(), field.name.clone()),
                        field.type_ref.name.clone(),
                    );
                }
                Member::Method(method) => {
                    self.methods.insert(
                        (declaration.name.clone(), method.name.clone()),
                        method.return_type.name.clone(),
                    );
                }
                Member::Property(property) => {
                    self.fields.insert(
                        (declaration.name.clone(), property.name.clone()),
                        property.type_ref.name.clone(),
                    );
                }
            }
        }
        let field_environment = declaration
            .members
            .iter()
            .filter_map(|member| match member {
                Member::Field(field) => Some((field.name.clone(), field.type_ref.name.clone())),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        for member in &mut declaration.members {
            match member {
                Member::Method(function) => {
                    if !function.type_parameters.is_empty() {
                        self.diagnostics.push(Diagnostic::error(
                            "generic methods are not implemented; methods may only use their owner type parameters",
                            function.span,
                        ));
                        continue;
                    }
                    let mut environment = field_environment.clone();
                    if !function.is_static {
                        environment.insert("this".to_owned(), declaration.name.clone());
                    }
                    environment.extend(function.parameters.iter().map(|parameter| {
                        (parameter.name.clone(), parameter.type_ref.name.clone())
                    }));
                    if let Some(body) = &mut function.body {
                        self.block(body, &mut environment);
                    }
                }
                Member::Field(field) => {
                    if let Some(initializer) = &mut field.initializer {
                        self.expression(initializer, &field_environment);
                    }
                }
                Member::Property(property) => {
                    if let Some(getter) = &mut property.getter {
                        self.block(&mut getter.body, &mut field_environment.clone());
                    }
                    if let Some(setter) = &mut property.setter {
                        let mut environment = field_environment.clone();
                        environment.insert("value".to_owned(), property.type_ref.name.clone());
                        self.block(&mut setter.body, &mut environment);
                    }
                }
            }
        }
    }

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

    fn block(&mut self, block: &mut Block, environment: &mut HashMap<String, String>) {
        for statement in &mut block.statements {
            self.statement(statement, environment);
        }
    }

    fn statement(&mut self, statement: &mut Statement, environment: &mut HashMap<String, String>) {
        match statement {
            Statement::Variable(variable) => {
                if let Some(value) = &mut variable.initializer {
                    let inferred = self.expression(value, environment);
                    let type_ = variable_type(variable).unwrap_or(inferred);
                    environment.insert(variable.name.clone(), type_);
                } else if let Some(type_) = variable_type(variable) {
                    environment.insert(variable.name.clone(), type_);
                }
            }
            Statement::Return { value, .. } => {
                if let Some(value) = value {
                    self.expression(value, environment);
                }
            }
            Statement::Expression(value) => {
                self.expression(value, environment);
            }
            Statement::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                self.expression(condition, environment);
                self.block(then_block, &mut environment.clone());
                if let Some(block) = else_block {
                    self.block(block, &mut environment.clone());
                }
            }
            Statement::While {
                condition, body, ..
            } => {
                self.expression(condition, environment);
                self.block(body, &mut environment.clone());
            }
            Statement::For {
                initializer,
                condition,
                update,
                body,
                ..
            } => {
                let mut loop_environment = environment.clone();
                if let Some(initializer) = initializer {
                    self.statement(initializer, &mut loop_environment);
                }
                if let Some(condition) = condition {
                    self.expression(condition, &loop_environment);
                }
                if let Some(update) = update {
                    self.expression(update, &loop_environment);
                }
                self.block(body, &mut loop_environment);
            }
            Statement::Switch {
                value,
                cases,
                default,
                ..
            } => {
                self.expression(value, environment);
                for case in cases {
                    self.block(&mut case.body, &mut environment.clone());
                }
                if let Some(default) = default {
                    self.block(default, &mut environment.clone());
                }
            }
            Statement::Break(_) | Statement::Continue(_) => {}
        }
    }

    fn expression(
        &mut self,
        expression: &mut Expression,
        environment: &HashMap<String, String>,
    ) -> String {
        match &mut expression.kind {
            ExpressionKind::Literal(value) => literal_type(value),
            ExpressionKind::Name(name) => environment.get(name).cloned().unwrap_or_default(),
            ExpressionKind::This => environment.get("this").cloned().unwrap_or_default(),
            ExpressionKind::StructLiteral { type_name, fields } => {
                for field in fields {
                    self.expression(&mut field.value, environment);
                }
                type_name.clone()
            }
            ExpressionKind::ArrayLiteral(values) => values
                .iter_mut()
                .map(|value| self.expression(value, environment))
                .find(|type_| !type_.is_empty())
                .map_or_else(String::new, |type_| format!("{type_}[]")),
            ExpressionKind::NewArray {
                element_type,
                length,
            } => {
                self.expression(length, environment);
                format!("{}[]", element_type.name)
            }
            ExpressionKind::NewObject {
                type_name,
                arguments,
            } => {
                for argument in arguments {
                    self.expression(argument, environment);
                }
                type_name.clone()
            }
            ExpressionKind::Index { array, index } => {
                let array = self.expression(array, environment);
                self.expression(index, environment);
                array.strip_suffix("[]").unwrap_or_default().to_owned()
            }
            ExpressionKind::Member { object, name } => {
                let owner = self.expression(object, environment);
                if name == "Length" && (owner.ends_with("[]") || owner == "string") {
                    "int".to_owned()
                } else {
                    self.fields
                        .get(&(owner, name.clone()))
                        .cloned()
                        .unwrap_or_default()
                }
            }
            ExpressionKind::Call {
                callee,
                type_arguments,
                arguments,
            } => self.call_expression(
                callee,
                type_arguments,
                arguments,
                expression.span,
                environment,
            ),
            ExpressionKind::Unary { operand, .. }
            | ExpressionKind::IncrementDecrement { operand, .. }
            | ExpressionKind::Try { operand } => self.expression(operand, environment),
            ExpressionKind::Conditional {
                condition,
                when_true,
                when_false,
            } => {
                self.expression(condition, environment);
                let first = self.expression(when_true, environment);
                let second = self.expression(when_false, environment);
                if first == second {
                    first
                } else {
                    String::new()
                }
            }
            ExpressionKind::Cast { target, operand } => {
                self.expression(operand, environment);
                target.name.clone()
            }
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => self.binary_expression(left, *operator, right, environment),
            ExpressionKind::Assignment { target, value, .. } => {
                let target = self.expression(target, environment);
                self.expression(value, environment);
                target
            }
        }
    }

    fn binary_expression(
        &mut self,
        left: &mut Expression,
        operator: BinaryOperator,
        right: &mut Expression,
        environment: &HashMap<String, String>,
    ) -> String {
        let left = self.expression(left, environment);
        self.expression(right, environment);
        if matches!(
            operator,
            BinaryOperator::Less
                | BinaryOperator::LessEqual
                | BinaryOperator::Greater
                | BinaryOperator::GreaterEqual
                | BinaryOperator::Equal
                | BinaryOperator::NotEqual
                | BinaryOperator::LogicalAnd
                | BinaryOperator::LogicalOr
        ) {
            "bool".to_owned()
        } else {
            left
        }
    }

    fn call_expression(
        &mut self,
        callee: &mut Expression,
        type_arguments: &mut Vec<TypeRef>,
        arguments: &mut [Expression],
        span: aster_diagnostics::Span,
        environment: &HashMap<String, String>,
    ) -> String {
        let argument_types = arguments
            .iter_mut()
            .map(|argument| self.expression(argument, environment))
            .collect::<Vec<_>>();
        if let ExpressionKind::Name(name) = &mut callee.kind
            && self.templates.contains_key(name)
        {
            let result = self.instantiate(name, type_arguments, &argument_types, span);
            type_arguments.clear();
            if let Some((specialized, return_type)) = result {
                *name = specialized;
                return return_type;
            }
            return String::new();
        }
        if !type_arguments.is_empty() {
            let name = match &callee.kind {
                ExpressionKind::Name(name) => name.as_str(),
                _ => "this callable",
            };
            self.diagnostics.push(
                Diagnostic::error(format!("`{name}` is not a generic function"), span)
                    .with_help("remove the explicit type arguments"),
            );
        }
        let callee_type = self.expression(callee, environment);
        match &callee.kind {
            ExpressionKind::Name(name) => self.returns.get(name).cloned().unwrap_or_default(),
            ExpressionKind::Member { object, name } => {
                let owner = self.infer_readonly(object, environment);
                self.methods
                    .get(&(owner, name.clone()))
                    .cloned()
                    .unwrap_or_default()
            }
            _ => callee_type,
        }
    }

    fn infer_readonly(
        &self,
        expression: &Expression,
        environment: &HashMap<String, String>,
    ) -> String {
        match &expression.kind {
            ExpressionKind::Name(name) => environment.get(name).cloned().unwrap_or_else(|| {
                if self.methods.keys().any(|(owner, _)| owner == name) {
                    name.clone()
                } else {
                    String::new()
                }
            }),
            ExpressionKind::This => environment.get("this").cloned().unwrap_or_default(),
            ExpressionKind::NewObject { type_name, .. }
            | ExpressionKind::StructLiteral { type_name, .. } => type_name.clone(),
            _ => String::new(),
        }
    }

    fn instantiate(
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

fn infer_type(
    pattern: &str,
    actual: &str,
    parameters: &[String],
    inferred: &mut HashMap<String, String>,
    span: aster_diagnostics::Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (Some(pattern), Some(actual)) = (TypeName::parse(pattern), TypeName::parse(actual)) else {
        return;
    };
    infer_type_name(&pattern, &actual, parameters, inferred, span, diagnostics);
}

fn infer_type_name(
    pattern: &TypeName,
    actual: &TypeName,
    parameters: &[String],
    inferred: &mut HashMap<String, String>,
    span: aster_diagnostics::Span,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if parameters
        .iter()
        .any(|parameter| parameter == &pattern.base)
        && pattern.arguments.is_empty()
        && !pattern.array
    {
        let concrete = actual.to_string();
        if let Some(previous) = inferred.get(&pattern.base) {
            if previous != &concrete {
                diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "conflicting inference for `{}`: `{previous}` and `{concrete}`",
                            pattern.base
                        ),
                        span,
                    )
                    .with_help("use matching argument types or explicit casts"),
                );
            }
        } else {
            inferred.insert(pattern.base.clone(), concrete);
        }
        return;
    }
    if parameters
        .iter()
        .any(|parameter| parameter == &pattern.base)
        && pattern.arguments.is_empty()
        && pattern.array
        && actual.array
    {
        let mut element = actual.clone();
        element.array = false;
        let concrete = element.to_string();
        if let Some(previous) = inferred.get(&pattern.base) {
            if previous != &concrete {
                diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "conflicting inference for `{}`: `{previous}` and `{concrete}`",
                            pattern.base
                        ),
                        span,
                    )
                    .with_help("use matching argument types or explicit casts"),
                );
            }
        } else {
            inferred.insert(pattern.base.clone(), concrete);
        }
        return;
    }
    if pattern.array != actual.array
        || pattern.base != actual.base
        || pattern.arguments.len() != actual.arguments.len()
    {
        return;
    }
    for (pattern, actual) in pattern.arguments.iter().zip(&actual.arguments) {
        infer_type_name(pattern, actual, parameters, inferred, span, diagnostics);
    }
}

fn substitutions(parameters: &[String], concrete: &[String]) -> HashMap<String, String> {
    parameters
        .iter()
        .cloned()
        .zip(concrete.iter().cloned())
        .collect()
}

fn substitute_name(name: &str, substitutions: &HashMap<String, String>) -> String {
    let Some(mut type_name) = TypeName::parse(name) else {
        return name.to_owned();
    };
    substitute_type_name(&mut type_name, substitutions);
    type_name.to_string()
}

fn substitute_type_name(type_name: &mut TypeName, substitutions: &HashMap<String, String>) {
    if type_name.arguments.is_empty()
        && let Some(replacement) = substitutions.get(&type_name.base)
        && let Some(mut replacement) = TypeName::parse(replacement)
    {
        replacement.array |= type_name.array;
        *type_name = replacement;
        return;
    }
    for argument in &mut type_name.arguments {
        substitute_type_name(argument, substitutions);
    }
}

struct TypeSubstituter<'a> {
    substitutions: &'a HashMap<String, String>,
}

impl<'a> TypeSubstituter<'a> {
    fn new(substitutions: &'a HashMap<String, String>) -> Self {
        Self { substitutions }
    }
}

impl AstVisitorMut for TypeSubstituter<'_> {
    fn visit_expression_mut(&mut self, expression: &mut Expression) {
        match &mut expression.kind {
            ExpressionKind::StructLiteral { type_name, .. }
            | ExpressionKind::NewObject { type_name, .. } => {
                *type_name = substitute_name(type_name, self.substitutions);
            }
            ExpressionKind::Name(name)
                if TypeName::parse(name)
                    .is_some_and(|type_name| !type_name.arguments.is_empty()) =>
            {
                *name = substitute_name(name, self.substitutions);
            }
            _ => {}
        }
        walk_expression_mut(self, expression);
    }

    fn visit_switch_case_mut(&mut self, case: &mut SwitchCase) {
        if let Some(owner) = &mut case.enum_name {
            *owner = substitute_name(owner, self.substitutions);
        }
        walk_switch_case_mut(self, case);
    }

    fn visit_type_ref_mut(&mut self, type_ref: &mut TypeRef) {
        type_ref.name = substitute_name(&type_ref.name, self.substitutions);
    }
}

struct GenericTypeConcretizer<'a> {
    monomorphizer: &'a mut Monomorphizer,
}

impl<'a> GenericTypeConcretizer<'a> {
    fn new(monomorphizer: &'a mut Monomorphizer) -> Self {
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
fn variable_type(variable: &VariableDeclaration) -> Option<String> {
    match &variable.kind {
        VariableKind::Explicit(type_) | VariableKind::Constant(type_) => Some(type_.name.clone()),
        VariableKind::Inferred => None,
    }
}
fn literal_type(value: &Literal) -> String {
    match value {
        Literal::Integer(value) => value.parse::<i32>().map_or("long", |_| "int"),
        Literal::Long(_) => "long",
        Literal::UInt(_) => "uint",
        Literal::ULong(_) => "ulong",
        Literal::Float(_) => "float",
        Literal::Double(_) => "double",
        Literal::Decimal(_) => "decimal",
        Literal::String(_) => "string",
        Literal::Character(_) => "char",
        Literal::Boolean(_) => "bool",
    }
    .to_owned()
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
