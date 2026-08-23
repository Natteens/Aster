use super::{
    BinaryOperator, Block, Diagnostic, Expression, ExpressionKind, FunctionDeclaration, HashMap,
    InterpolatedPart, Member, Monomorphizer, Statement, TypeDeclaration, TypeName, TypeRef,
    literal_type, variable_type,
};

/// Every built-in `string.TryParse*()` method name, paired with the concrete
/// primitive its `Option<T>` specialization must exist for. A fixed method
/// call name in source is enough to request the specialization eagerly here,
/// even when the receiver's own type cannot be inferred by this pass's
/// best-effort textual tracking (e.g. `helper().TryParseInt()`); the real
/// receiver-type gate is semantic analysis's `receiver == Type::String`
/// check, not this discovery pass.
const TRY_PARSE_TARGETS: [(&str, &str); 7] = [
    ("TryParseBool", "bool"),
    ("TryParseInt", "int"),
    ("TryParseUInt", "uint"),
    ("TryParseLong", "long"),
    ("TryParseULong", "ulong"),
    ("TryParseFloat", "float"),
    ("TryParseDouble", "double"),
];

impl Monomorphizer {
    pub(super) fn function(&mut self, function: &mut FunctionDeclaration) {
        let mut environment = function
            .parameters
            .iter()
            .map(|parameter| (parameter.name.clone(), parameter.type_ref.name.clone()))
            .collect();
        if let Some(body) = &mut function.body {
            self.block(body, &mut environment);
        }
    }

    pub(super) fn analyze_type_declaration(&mut self, declaration: &mut TypeDeclaration) {
        // Generated class specializations reach this point too, which is how
        // `Wrapper<int>` keeps the interface relation declared by `Wrapper<T>`
        // when it is later used as a constrained type argument.
        if !declaration.interfaces.is_empty() {
            self.class_interfaces.insert(
                declaration.name.clone(),
                declaration
                    .interfaces
                    .iter()
                    .map(|interface| interface.name.clone())
                    .collect(),
            );
        }
        for member in &declaration.members {
            match member {
                Member::Field(field) => {
                    self.fields.insert(
                        (declaration.name.clone(), field.name.clone()),
                        field.type_ref.name.clone(),
                    );
                }
                Member::Method(method) => {
                    let key = (declaration.name.clone(), method.name.clone());
                    self.methods
                        .insert(key.clone(), method.return_type.name.clone());
                    let signature = method
                        .parameters
                        .iter()
                        .map(|parameter| parameter.type_ref.name.clone())
                        .collect::<Vec<_>>();
                    let signatures = self.method_signatures.entry(key).or_default();
                    if !signatures.contains(&signature) {
                        signatures.push(signature);
                    }
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
                    self.analyze_method(&declaration.name, function, &field_environment);
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

    pub(super) fn analyze_method(
        &mut self,
        owner: &str,
        function: &mut FunctionDeclaration,
        fields: &HashMap<String, String>,
    ) {
        let mut environment = fields.clone();
        environment.insert("#owner".to_owned(), owner.to_owned());
        if !function.is_static {
            environment.insert("this".to_owned(), owner.to_owned());
        }
        environment.extend(
            function
                .parameters
                .iter()
                .map(|parameter| (parameter.name.clone(), parameter.type_ref.name.clone())),
        );
        if let Some(body) = &mut function.body {
            self.block(body, &mut environment);
        }
    }

    fn block(&mut self, block: &mut Block, environment: &mut HashMap<String, String>) {
        for statement in &mut block.statements {
            self.statement(statement, environment);
        }
    }

    #[allow(clippy::too_many_lines)] // keeps one exhaustive statement traversal with one environment
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
            Statement::ForEach {
                element_type,
                element_name,
                collection,
                body,
                ..
            } => {
                if let Some(element_type) = element_type
                    && let Some(concrete) = environment.get(&element_type.name)
                {
                    element_type.name.clone_from(concrete);
                }
                let collection_type = self.expression(collection, environment);
                let inferred_element = element_type.as_ref().map_or_else(
                    || {
                        collection_type
                            .strip_suffix("[]")
                            .map(str::to_owned)
                            .or_else(|| {
                                TypeName::parse(&collection_type).and_then(|type_name| {
                                    (type_name.base == "List" && type_name.arguments.len() == 1)
                                        .then(|| type_name.arguments[0].to_string())
                                })
                            })
                            .unwrap_or_else(|| {
                                if collection_type == "string" {
                                    "char".to_owned()
                                } else {
                                    String::new()
                                }
                            })
                    },
                    |type_ref| type_ref.name.clone(),
                );
                let mut loop_environment = environment.clone();
                loop_environment.insert(element_name.clone(), inferred_element);
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
            Statement::Unsafe { body, .. } => self.block(body, &mut environment.clone()),
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
                    self.expression(&mut argument.value, environment);
                }
                type_name.clone().unwrap_or_default()
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
            | ExpressionKind::Try { operand }
            | ExpressionKind::Await { operand } => self.expression(operand, environment),
            ExpressionKind::Conditional {
                condition,
                when_true,
                when_false,
            } => self.conditional_expression(condition, when_true, when_false, environment),
            ExpressionKind::Switch {
                value,
                cases,
                default,
            } => self.switch_expression(value, cases, default, environment),
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
            ExpressionKind::InterpolatedString { parts } => {
                for part in parts {
                    if let InterpolatedPart::Expression(expression) = part {
                        self.expression(expression, environment);
                    }
                }
                "string".to_owned()
            }
        }
    }

    fn conditional_expression(
        &mut self,
        condition: &mut Expression,
        when_true: &mut Expression,
        when_false: &mut Expression,
        environment: &HashMap<String, String>,
    ) -> String {
        self.expression(condition, environment);
        let first = self.expression(when_true, environment);
        let second = self.expression(when_false, environment);
        if first == second {
            first
        } else {
            String::new()
        }
    }

    fn switch_expression(
        &mut self,
        value: &mut Expression,
        cases: &mut [aster_syntax::SwitchExpressionCase],
        default: &mut Option<Box<Expression>>,
        environment: &HashMap<String, String>,
    ) -> String {
        self.expression(value, environment);
        let mut result = String::new();
        for case in cases {
            let arm = self.expression(&mut case.value, environment);
            if result.is_empty() {
                result = arm;
            } else if result != arm {
                result.clear();
            }
        }
        if let Some(default) = default {
            let arm = self.expression(default, environment);
            if result.is_empty() {
                result = arm;
            } else if result != arm {
                result.clear();
            }
        }
        result
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

    // Call discovery keeps generic free functions, generic methods, built-ins, and ordinary
    // callable inference in one precedence-ordered routine so no second overload engine emerges.
    #[allow(clippy::too_many_lines)]
    fn call_expression(
        &mut self,
        callee: &mut Expression,
        type_arguments: &mut Vec<TypeRef>,
        arguments: &mut [aster_syntax::Argument],
        span: aster_diagnostics::Span,
        environment: &HashMap<String, String>,
    ) -> String {
        // `Task.Run(Target, values...)` carries a callable request without
        // syntactically invoking `Target`. Reuse this exact call-discovery
        // routine on that target and its runtime values so generic inference,
        // overload selection, constraints, specialization identity, cache
        // rollback, and concrete name rewriting all remain owned here. The
        // later semantic Task gate still proves that the resulting callable is
        // static/free and worker-transferable.
        if is_task_run_callee(callee)
            && let Some((target, values)) = arguments.split_first_mut()
        {
            let result = self.call_expression(
                &mut target.value,
                &mut Vec::new(),
                values,
                target.span,
                environment,
            );
            return if result.is_empty() {
                String::new()
            } else {
                format!("Task<{result}>")
            };
        }
        let argument_types = arguments
            .iter_mut()
            .map(|argument| self.expression(&mut argument.value, environment))
            .collect::<Vec<_>>();
        if let ExpressionKind::Name(name) = &mut callee.kind
            && self.templates.contains_key(name)
        {
            let template = self.templates[name].clone();
            let ordered = ordered_generic_argument_types(&template, arguments, &argument_types)
                .unwrap_or_else(|| argument_types.clone());
            let result = self.instantiate(name, type_arguments, &ordered, span);
            type_arguments.clear();
            if let Some((specialized, return_type)) = result {
                *name = specialized;
                return return_type;
            }
            return String::new();
        }
        let method_request = match &callee.kind {
            ExpressionKind::Member { object, name } => {
                Some((self.infer_readonly(object, environment), name.clone()))
            }
            ExpressionKind::Name(name) => environment
                .get("#owner")
                .map(|owner| (owner.clone(), name.clone())),
            _ => None,
        };
        if let Some((owner, source_name)) = method_request
            && let Some(templates) = self
                .method_templates
                .get(&(owner.clone(), source_name.clone()))
                .cloned()
        {
            let explicit_method_arguments = !type_arguments.is_empty();
            let ordinary_signatures = self
                .method_signatures
                .get(&(owner.clone(), source_name.clone()))
                .cloned()
                .unwrap_or_default();
            let ordinary_exact = ordinary_signatures
                .iter()
                .any(|parameters| parameters == &argument_types);
            let mut candidates = if type_arguments.is_empty() {
                let inferable = templates
                    .iter()
                    .filter(|template| {
                        method_inference_possible(template, arguments, &argument_types)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let exact = inferable
                    .iter()
                    .filter(|template| {
                        method_inferred_signature_exact(template, arguments, &argument_types)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if exact.is_empty() { inferable } else { exact }
            } else {
                let matching_arity = templates
                    .iter()
                    .filter(|template| template.type_parameters.len() == type_arguments.len())
                    .cloned()
                    .collect::<Vec<_>>();
                let concrete = type_arguments
                    .iter()
                    .map(|argument| argument.name.clone())
                    .collect::<Vec<_>>();
                let exact = matching_arity
                    .iter()
                    .filter(|template| {
                        ordered_generic_argument_types(template, arguments, &argument_types)
                            .is_some_and(|ordered| {
                                method_signature_exact(template, &concrete, &ordered)
                            })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if exact.is_empty() {
                    matching_arity
                } else {
                    exact
                }
            };
            if candidates.len() > 1 {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("call to generic method `{owner}.{source_name}` is ambiguous"),
                        span,
                    )
                    .with_help("use a distinct method name or parameter signature"),
                );
                type_arguments.clear();
                return String::new();
            }
            if candidates.is_empty() {
                if type_arguments.is_empty() && !ordinary_signatures.is_empty() {
                    let callee_type = self.expression(callee, environment);
                    return match &callee.kind {
                        ExpressionKind::Member { object, name } => {
                            let owner = self.infer_readonly(object, environment);
                            self.methods
                                .get(&(owner, name.clone()))
                                .cloned()
                                .unwrap_or(callee_type)
                        }
                        _ => callee_type,
                    };
                }
                if type_arguments.is_empty() {
                    candidates.push(templates[0].clone());
                } else {
                    self.diagnostics.push(Diagnostic::error(
                        format!(
                            "generic method `{owner}.{source_name}` has no overload with {} type argument(s)",
                            type_arguments.len()
                        ),
                        span,
                    ));
                    type_arguments.clear();
                    return String::new();
                }
            }
            let ordered =
                ordered_generic_argument_types(&candidates[0], arguments, &argument_types)
                    .unwrap_or_else(|| argument_types.clone());
            let result =
                self.instantiate_method(&owner, &candidates[0], type_arguments, &ordered, span);
            type_arguments.clear();
            if let Some((specialized, return_type)) = result {
                let specialized_exact = self
                    .method_signatures
                    .get(&(owner.clone(), specialized.clone()))
                    .is_some_and(|signatures| {
                        signatures
                            .iter()
                            .any(|parameters| parameters == &argument_types)
                    });
                if !explicit_method_arguments && ordinary_exact && specialized_exact {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("call to method `{owner}.{source_name}` is ambiguous"),
                            span,
                        )
                        .with_help("provide explicit type arguments to select the generic method"),
                    );
                    return String::new();
                }
                if explicit_method_arguments || !ordinary_exact || !specialized_exact {
                    match &mut callee.kind {
                        ExpressionKind::Member { name, .. } | ExpressionKind::Name(name) => {
                            *name = specialized;
                        }
                        _ => unreachable!("generic method call has a name or member callee"),
                    }
                    return return_type;
                }
            }
            if ordinary_exact {
                return self
                    .methods
                    .get(&(owner, source_name))
                    .cloned()
                    .unwrap_or_default();
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
                if arguments.is_empty()
                    && let Some((_, target)) =
                        TRY_PARSE_TARGETS.iter().find(|(method, _)| method == name)
                {
                    let option_base =
                        format!("{}::Option", crate::standard_library::CORE_NAMESPACE);
                    let option_type = crate::standard_library::option_specialization_name(target);
                    // `Option` may be absent (no `using aster.core;`); leave
                    // that as a normal, reported diagnostic from semantic
                    // analysis's own existence check rather than panicking
                    // here on a missing template.
                    if self.enum_templates.contains_key(&option_base)
                        && let Some(concrete) = TypeName::parse(&option_type)
                    {
                        self.instantiate_type(&concrete.base, &concrete.arguments, span);
                    }
                    return option_type;
                }
                let owner = self.infer_readonly(object, environment);
                if let Some(dictionary) = TypeName::parse(&owner)
                    && (dictionary.base == "Dictionary"
                        || dictionary.base
                            == format!(
                                "{}::Dictionary",
                                crate::standard_library::COLLECTIONS_NAMESPACE
                            ))
                    && dictionary.arguments.len() == 2
                {
                    let key = dictionary.arguments[0].to_string();
                    let value = dictionary.arguments[1].to_string();
                    if name == "TryGet" {
                        let option_base =
                            format!("{}::Option", crate::standard_library::CORE_NAMESPACE);
                        let option_type =
                            crate::standard_library::option_specialization_name(&value);
                        if self.enum_templates.contains_key(&option_base)
                            && let Some(concrete) = TypeName::parse(&option_type)
                        {
                            self.instantiate_type(&concrete.base, &concrete.arguments, span);
                        }
                        return option_type;
                    }
                    if name == "Entries" {
                        let entry_type =
                            crate::standard_library::dictionary_entry_specialization_name(
                                &key, &value,
                            );
                        if let Some(concrete) = TypeName::parse(&entry_type)
                            && self.type_templates.contains_key(&concrete.base)
                        {
                            self.instantiate_type(&concrete.base, &concrete.arguments, span);
                        }
                        return format!("{entry_type}[]");
                    }
                }
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
                if self.methods.keys().any(|(owner, _)| owner == name)
                    || self.method_templates.keys().any(|(owner, _)| owner == name)
                {
                    name.clone()
                } else {
                    String::new()
                }
            }),
            ExpressionKind::This => environment.get("this").cloned().unwrap_or_default(),
            ExpressionKind::NewObject { type_name, .. } => type_name.clone().unwrap_or_default(),
            ExpressionKind::StructLiteral { type_name, .. } => type_name.clone(),
            _ => String::new(),
        }
    }
}

fn is_task_run_callee(callee: &Expression) -> bool {
    matches!(
        &callee.kind,
        ExpressionKind::Member { object, name }
            if name == "Run"
                && matches!(&object.kind, ExpressionKind::Name(owner) if owner == "Task")
    )
}

fn method_inference_possible(
    template: &FunctionDeclaration,
    source_arguments: &[aster_syntax::Argument],
    arguments: &[String],
) -> bool {
    ordered_generic_argument_types(template, source_arguments, arguments)
        .and_then(|ordered| infer_method_arguments(template, &ordered))
        .is_some()
}

fn method_inferred_signature_exact(
    template: &FunctionDeclaration,
    source_arguments: &[aster_syntax::Argument],
    arguments: &[String],
) -> bool {
    ordered_generic_argument_types(template, source_arguments, arguments).is_some_and(|ordered| {
        infer_method_arguments(template, &ordered)
            .is_some_and(|concrete| method_signature_exact(template, &concrete, &ordered))
    })
}

fn infer_method_arguments(
    template: &FunctionDeclaration,
    arguments: &[String],
) -> Option<Vec<String>> {
    let parameters = template
        .type_parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>();
    let mut inferred = HashMap::new();
    let mut diagnostics = Vec::new();
    for (parameter, actual) in template.parameters.iter().zip(arguments) {
        if actual.is_empty() {
            continue;
        }
        super::infer_type(
            &parameter.type_ref.name,
            actual,
            &parameters,
            &mut inferred,
            template.span,
            &mut diagnostics,
        );
    }
    (diagnostics.is_empty()
        && parameters
            .iter()
            .all(|parameter| inferred.contains_key(parameter)))
    .then(|| {
        parameters
            .iter()
            .map(|parameter| inferred[parameter].clone())
            .collect()
    })
}

fn method_signature_exact(
    template: &FunctionDeclaration,
    concrete: &[String],
    arguments: &[String],
) -> bool {
    if template.parameters.len() != arguments.len() {
        return false;
    }
    let parameters = template
        .type_parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>();
    let substitutions = super::substitutions(&parameters, concrete);
    template
        .parameters
        .iter()
        .zip(arguments)
        .all(|(parameter, argument)| {
            argument.is_empty()
                || super::substitute_name(&parameter.type_ref.name, &substitutions) == *argument
        })
}

fn ordered_generic_argument_types(
    template: &FunctionDeclaration,
    arguments: &[aster_syntax::Argument],
    types: &[String],
) -> Option<Vec<String>> {
    let mut ordered = vec![String::new(); template.parameters.len()];
    let mut occupied = vec![false; template.parameters.len()];
    let mut next = 0usize;
    let mut saw_named = false;
    for (argument, type_) in arguments.iter().zip(types) {
        let parameter = if let Some(name) = &argument.name {
            saw_named = true;
            template
                .parameters
                .iter()
                .position(|parameter| parameter.name == *name)?
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
        ordered[parameter].clone_from(type_);
    }
    if occupied
        .iter()
        .enumerate()
        .any(|(index, supplied)| !supplied && template.parameters[index].default.is_none())
    {
        return None;
    }
    Some(ordered)
}
