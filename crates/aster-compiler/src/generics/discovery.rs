use std::collections::{HashSet, VecDeque};

use super::{
    BinaryOperator, Block, DeclarationKind, Diagnostic, Expression, ExpressionKind,
    FunctionDeclaration, HashMap, InterpolatedPart, Member, Monomorphizer, Statement,
    TypeDeclaration, TypeName, TypeRef, literal_type, variable_type,
};

/// Every built-in `string.TryParse*()` method name, paired with the concrete
/// primitive its `Option<T>` specialization must exist for. A fixed method
/// call name in source is enough to request the specialization eagerly here,
/// even when the receiver's own type cannot be inferred by this pass's
/// best-effort textual tracking (e.g. `helper().TryParseInt()`); the real
/// receiver-type gate is semantic analysis's `receiver == Type::String`
/// check, not this discovery pass.
const TRY_PARSE_TARGETS: [(&str, &str); 12] = [
    ("TryParseBool", "bool"),
    ("TryParseChar", "char"),
    ("TryParseSByte", "sbyte"),
    ("TryParseByte", "byte"),
    ("TryParseShort", "short"),
    ("TryParseUShort", "ushort"),
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
            let bases = declaration
                .interfaces
                .iter()
                .map(|interface| interface.name.clone())
                .collect::<Vec<_>>();
            self.type_bases
                .insert(declaration.name.clone(), bases.clone());
            self.class_interfaces
                .insert(declaration.name.clone(), bases);
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
        let field_environment = self.collect_owner_fields(&declaration.name);
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
                        let mut environment = field_environment.clone();
                        environment.insert("#owner".to_owned(), declaration.name.clone());
                        environment.insert("this".to_owned(), declaration.name.clone());
                        self.block(&mut getter.body, &mut environment);
                    }
                    if let Some(setter) = &mut property.setter {
                        let mut environment = field_environment.clone();
                        environment.insert("#owner".to_owned(), declaration.name.clone());
                        environment.insert("this".to_owned(), declaration.name.clone());
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
            Statement::DoWhile {
                body, condition, ..
            } => {
                self.block(body, &mut environment.clone());
                self.expression(condition, environment);
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
                let selected = self.expression(value, environment);
                for case in cases {
                    let mut case_env = environment.clone();
                    if let Some(fields) = self.enum_case_fields(&selected, &case.case_name) {
                        for (binding, field_type) in case.bindings.iter().zip(fields) {
                            case_env.insert(binding.clone(), field_type);
                        }
                    }
                    if let Some(guard) = &mut case.guard {
                        self.expression(guard, &case_env);
                    }
                    self.block(&mut case.body, &mut case_env);
                }
                if let Some(default) = default {
                    self.block(default, &mut environment.clone());
                }
            }
            Statement::Unsafe { body, .. } => self.block(body, &mut environment.clone()),
            Statement::Break(_) | Statement::Continue(_) => {}
        }
    }

    #[allow(clippy::too_many_lines)] // one exhaustive traversal of every expression form
    fn expression(
        &mut self,
        expression: &mut Expression,
        environment: &HashMap<String, String>,
    ) -> String {
        match &mut expression.kind {
            ExpressionKind::Literal(value) => literal_type(value),
            ExpressionKind::Name(name) => environment.get(name).cloned().unwrap_or_default(),
            ExpressionKind::This => environment.get("this").cloned().unwrap_or_default(),
            ExpressionKind::Base => self.base_class_of(environment).unwrap_or_default(),
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
            ExpressionKind::ObjectInitializer { creation, members } => {
                let type_ = self.expression(creation, environment);
                for member in members {
                    self.expression(&mut member.value, environment);
                }
                type_
            }
            ExpressionKind::Index { array, index } => {
                let owner = self.expression(array, environment);
                self.expression(index, environment);
                Self::index_result_type(&owner)
            }
            ExpressionKind::Member { object, name } => {
                let mut owner = self.expression(object, environment);
                if let ExpressionKind::Name(enum_name) = &object.kind
                    && !environment.contains_key(enum_name)
                    && self
                        .enum_case_fields(enum_name, name)
                        .is_some_and(|fields| fields.is_empty())
                {
                    return enum_name.clone();
                }
                if owner.is_empty() {
                    owner = self.infer_readonly(object, environment);
                }
                self.member_result_type(&owner, name)
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
            ExpressionKind::Unary { operator, operand } => {
                let inner = self.expression(operand, environment);
                if *operator == aster_syntax::UnaryOperator::Not {
                    "bool".to_owned()
                } else {
                    inner
                }
            }
            ExpressionKind::IncrementDecrement { operand, .. } => {
                self.expression(operand, environment)
            }
            ExpressionKind::Try { operand } => {
                let inner = self.expression(operand, environment);
                Self::unwrap_try_name(&inner).unwrap_or(inner)
            }
            ExpressionKind::Await { operand } => {
                let inner = self.expression(operand, environment);
                Self::unwrap_task_name(&inner).unwrap_or(inner)
            }
            ExpressionKind::NullCoalesce { value, fallback } => {
                let left = self.expression(value, environment);
                let right = self.expression(fallback, environment);
                Self::null_coalesce_result(&left, &right, fallback)
            }
            ExpressionKind::NullConditional { receiver, access } => {
                let receiver_type = self.expression(receiver, environment);
                let inner = Self::unwrap_option_name(&receiver_type).unwrap_or_default();
                self.null_conditional_receiver_types.push(inner);
                let tail_type = self.expression(access, environment);
                self.null_conditional_receiver_types.pop();
                if tail_type.is_empty()
                    || tail_type == "void"
                    || Self::unwrap_option_name(&tail_type).is_some()
                {
                    tail_type
                } else {
                    self.type_as_result(&tail_type, expression.span)
                }
            }
            ExpressionKind::NullConditionalReceiver => self
                .null_conditional_receiver_types
                .last()
                .cloned()
                .unwrap_or_default(),
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
            ExpressionKind::TypePattern { value, .. } => {
                self.expression(value, environment);
                "bool".to_owned()
            }
            ExpressionKind::TypeAs { value, target } => {
                self.expression(value, environment);
                self.type_as_result(&target.name, expression.span)
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

    /// `value as T` yields `Option<T>`: request that specialization exactly
    /// like the `TryParse*` and `Dictionary.TryGet` results, and return its name.
    fn type_as_result(&mut self, target: &str, span: aster_diagnostics::Span) -> String {
        let option_base = format!("{}::Option", crate::standard_library::CORE_NAMESPACE);
        let option_type = crate::standard_library::option_specialization_name(target);
        if self.enum_templates.contains_key(&option_base)
            && let Some(concrete) = TypeName::parse(&option_type)
        {
            self.instantiate_type(&concrete.base, &concrete.arguments, span);
        }
        option_type
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
        let first_is_null = matches!(
            when_true.kind,
            ExpressionKind::Literal(aster_syntax::Literal::Null)
        );
        let second_is_null = matches!(
            when_false.kind,
            ExpressionKind::Literal(aster_syntax::Literal::Null)
        );
        self.combine_branch_types(&first, first_is_null, &second, second_is_null)
            .0
    }

    fn switch_expression(
        &mut self,
        value: &mut Expression,
        cases: &mut [aster_syntax::SwitchExpressionCase],
        default: &mut Option<Box<Expression>>,
        environment: &HashMap<String, String>,
    ) -> String {
        let selected = self.expression(value, environment);
        let mut result: Option<(String, bool)> = None;
        for case in cases {
            let mut case_env = environment.clone();
            if let Some(fields) = self.enum_case_fields(&selected, &case.case_name) {
                for (binding, field_type) in case.bindings.iter().zip(fields) {
                    case_env.insert(binding.clone(), field_type);
                }
            }
            if let Some(guard) = &mut case.guard {
                self.expression(guard, &case_env);
            }
            let arm = self.expression(&mut case.value, &case_env);
            let is_null = matches!(
                case.value.kind,
                ExpressionKind::Literal(aster_syntax::Literal::Null)
            );
            result = Some(match result {
                None => (arm, is_null),
                Some((current, current_is_null)) => {
                    self.combine_branch_types(&current, current_is_null, &arm, is_null)
                }
            });
        }
        if let Some(default) = default {
            let arm = self.expression(default, environment);
            let is_null = matches!(
                default.kind,
                ExpressionKind::Literal(aster_syntax::Literal::Null)
            );
            result = Some(match result {
                None => (arm, is_null),
                Some((current, current_is_null)) => {
                    self.combine_branch_types(&current, current_is_null, &arm, is_null)
                }
            });
        }
        result.map_or_else(String::new, |(type_, _)| type_)
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
        if let Some((receiver_owner, source_name)) = method_request
            && let Some((owner, templates)) =
                self.resolve_method_templates(&receiver_owner, &source_name)
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
                        super::codes::GENERIC_AMBIGUOUS_METHOD_CALL,
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
                            let resolved = self.lookup_method_return(&owner, name);
                            if resolved.is_empty() {
                                callee_type
                            } else {
                                resolved
                            }
                        }
                        _ => callee_type,
                    };
                }
                if type_arguments.is_empty() {
                    candidates.push(templates[0].clone());
                } else {
                    self.diagnostics.push(Diagnostic::error(
                        super::codes::GENERIC_OVERLOAD_ARITY_MISMATCH,
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
                            super::codes::GENERIC_AMBIGUOUS_OVERLOAD_METHOD,
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
                Diagnostic::error(
                    super::codes::GENERIC_NOT_A_GENERIC_FUNCTION,
                    format!("`{name}` is not a generic function"),
                    span,
                )
                .with_help("remove the explicit type arguments"),
            );
        }
        let callee_type = self.expression(callee, environment);
        match &callee.kind {
            ExpressionKind::Name(name) => environment
                .get("#owner")
                .map(|owner| self.lookup_method_return(owner, name))
                .filter(|result| !result.is_empty())
                .or_else(|| self.returns.get(name).cloned())
                .unwrap_or_default(),
            ExpressionKind::Member { object, name } => {
                if let ExpressionKind::Name(enum_name) = &object.kind
                    && !environment.contains_key(enum_name)
                    && self.enum_case_fields(enum_name, name).is_some()
                {
                    return enum_name.clone();
                }
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
                if let Some(result) = Self::builtin_method_result(&owner, name) {
                    return result;
                }
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
                self.lookup_method_return(&owner, name)
            }
            _ => callee_type,
        }
    }

    fn unwrap_option_name(name: &str) -> Option<String> {
        let parsed = TypeName::parse(name)?;
        let option_base = format!("{}::Option", crate::standard_library::CORE_NAMESPACE);
        ((parsed.base == option_base || parsed.base == "?Option")
            && parsed.array_depth == 0
            && parsed.arguments.len() == 1)
            .then(|| parsed.arguments[0].to_string())
    }

    fn unwrap_try_name(name: &str) -> Option<String> {
        if let Some(inner) = Self::unwrap_option_name(name) {
            return Some(inner);
        }
        let parsed = TypeName::parse(name)?;
        let result_base = format!("{}::Result", crate::standard_library::CORE_NAMESPACE);
        (parsed.base == result_base && parsed.array_depth == 0 && parsed.arguments.len() == 2)
            .then(|| parsed.arguments[0].to_string())
    }

    fn unwrap_task_name(name: &str) -> Option<String> {
        let parsed = TypeName::parse(name)?;
        (parsed.base == "Task" && parsed.array_depth == 0 && parsed.arguments.len() == 1)
            .then(|| parsed.arguments[0].to_string())
    }

    fn null_coalesce_result(left: &str, right: &str, fallback: &Expression) -> String {
        if let Some(inner) = Self::unwrap_option_name(left) {
            if matches!(
                fallback.kind,
                ExpressionKind::Literal(aster_syntax::Literal::Null)
            ) {
                if Self::unwrap_option_name(&inner).is_some() {
                    inner
                } else {
                    left.to_owned()
                }
            } else if right == left {
                left.to_owned()
            } else {
                inner
            }
        } else {
            right.to_owned()
        }
    }

    fn is_enum_type_name(&self, name: &str) -> bool {
        if self.enums.contains_key(name) {
            return true;
        }
        let Some(parsed) = TypeName::parse(name) else {
            return false;
        };
        parsed.array_depth == 0
            && self
                .enum_templates
                .get(&parsed.base)
                .is_some_and(|template| template.type_parameters.len() == parsed.arguments.len())
    }

    fn enum_case_fields(&self, enum_name: &str, case_name: &str) -> Option<Vec<String>> {
        if enum_name.is_empty() {
            return None;
        }
        if let Some(declaration) = self.enums.get(enum_name) {
            let case = declaration
                .cases
                .iter()
                .find(|case| case.name == case_name)?;
            return Some(
                case.fields
                    .iter()
                    .map(|field| field.type_ref.name.clone())
                    .collect(),
            );
        }
        let parsed = TypeName::parse(enum_name)?;
        if parsed.array_depth != 0 {
            return None;
        }
        let template = self.enum_templates.get(&parsed.base)?;
        if template.type_parameters.len() != parsed.arguments.len() {
            return None;
        }
        let case = template.cases.iter().find(|case| case.name == case_name)?;
        let parameter_names = template
            .type_parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let concrete = parsed
            .arguments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let substitutions = super::substitutions(&parameter_names, &concrete);
        Some(
            case.fields
                .iter()
                .map(|field| super::substitute_name(&field.type_ref.name, &substitutions))
                .collect(),
        )
    }

    fn combine_branch_types(
        &self,
        left: &str,
        left_is_null: bool,
        right: &str,
        right_is_null: bool,
    ) -> (String, bool) {
        if left_is_null && right_is_null {
            return (String::new(), true);
        }
        if left_is_null {
            return if Self::unwrap_option_name(right).is_some() {
                (right.to_owned(), false)
            } else {
                (String::new(), false)
            };
        }
        if right_is_null {
            return if Self::unwrap_option_name(left).is_some() {
                (left.to_owned(), false)
            } else {
                (String::new(), false)
            };
        }
        (self.common_branch_type(left, right), false)
    }

    fn common_branch_type(&self, left: &str, right: &str) -> String {
        if left.is_empty() || right.is_empty() {
            return String::new();
        }
        if left == right {
            return left.to_owned();
        }
        if let (Some(l), Some(r)) = (
            crate::primitives::from_name(left),
            crate::primitives::from_name(right),
        ) && let Some(promoted) = crate::primitives::promote(l, r)
        {
            return promoted.name().to_owned();
        }
        if self.is_compatible_branch(left, right) {
            return left.to_owned();
        }
        if self.is_compatible_branch(right, left) {
            return right.to_owned();
        }
        String::new()
    }

    fn is_compatible_branch(&self, expected: &str, actual: &str) -> bool {
        if expected == actual {
            return true;
        }
        if let (Some(e), Some(a)) = (
            crate::primitives::from_name(expected),
            crate::primitives::from_name(actual),
        ) && crate::primitives::implicit_converts(a, e)
        {
            return true;
        }
        if self.derives_or_implements(actual, expected) {
            return true;
        }
        if let Some(inner) = Self::unwrap_option_name(expected) {
            return actual == inner
                || (Self::unwrap_option_name(&inner).is_none()
                    && self.is_compatible_branch(&inner, actual));
        }
        false
    }

    fn derives_or_implements(&self, derived: &str, base: &str) -> bool {
        if derived.is_empty() || base.is_empty() {
            return false;
        }
        let mut queue = VecDeque::from([derived.to_owned()]);
        let mut visited = HashSet::new();
        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            for next in self.next_bases(&current) {
                if next == base {
                    return true;
                }
                if !visited.contains(&next) {
                    queue.push_back(next);
                }
            }
        }
        false
    }

    fn index_result_type(owner: &str) -> String {
        if owner == "string" {
            return "char".to_owned();
        }
        let Some(parsed) = TypeName::parse(owner) else {
            return String::new();
        };
        if parsed.array_depth > 0 {
            let mut inner = parsed;
            inner.array_depth -= 1;
            return inner.to_string();
        }
        if parsed.base == "List" && parsed.arguments.len() == 1 {
            return parsed.arguments[0].to_string();
        }
        if (parsed.base == "Dictionary"
            || parsed.base
                == format!(
                    "{}::Dictionary",
                    crate::standard_library::COLLECTIONS_NAMESPACE
                ))
            && parsed.arguments.len() == 2
        {
            return parsed.arguments[1].to_string();
        }
        String::new()
    }

    fn member_result_type(&self, owner: &str, name: &str) -> String {
        if owner == "string" && name == "Length" {
            return "int".to_owned();
        }
        if let Some(parsed) = TypeName::parse(owner) {
            if parsed.array_depth > 0 && name == "Length" {
                return "int".to_owned();
            }
            if parsed.array_depth == 0
                && (parsed.base == "List"
                    || parsed.base == "Dictionary"
                    || parsed.base
                        == format!(
                            "{}::Dictionary",
                            crate::standard_library::COLLECTIONS_NAMESPACE
                        ))
                && matches!(name, "Length" | "Capacity")
            {
                return "int".to_owned();
            }
        }
        self.lookup_field_type(owner, name)
    }

    fn builtin_method_result(owner: &str, name: &str) -> Option<String> {
        if matches!(
            owner,
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
                | "float"
                | "double"
        ) {
            return (name == "ToString").then(|| "string".to_owned());
        }
        if owner == "string" {
            return match name {
                "Substring" | "Trim" | "TrimStart" | "TrimEnd" | "Replace" | "ToUpper"
                | "ToLower" => Some("string".to_owned()),
                "Contains" | "StartsWith" | "EndsWith" | "IsNullOrEmpty" => Some("bool".to_owned()),
                "IndexOf" | "LastIndexOf" => Some("int".to_owned()),
                "Split" => Some("string[]".to_owned()),
                _ => None,
            };
        }
        let parsed = TypeName::parse(owner)?;
        if parsed.array_depth == 0 && parsed.base == "List" && parsed.arguments.len() == 1 {
            let elem = parsed.arguments[0].to_string();
            return match name {
                "Add" | "Clear" | "RemoveAt" | "Set" | "AddRange" | "Insert" | "RemoveRange"
                | "Reverse" => Some("void".to_owned()),
                "Get" => Some(elem),
                "Contains" | "Remove" => Some("bool".to_owned()),
                "IndexOf" | "EnsureCapacity" => Some("int".to_owned()),
                "ToArray" | "GetRange" => Some(format!("{elem}[]")),
                _ => None,
            };
        }
        if parsed.array_depth == 0
            && (parsed.base == "Dictionary"
                || parsed.base
                    == format!(
                        "{}::Dictionary",
                        crate::standard_library::COLLECTIONS_NAMESPACE
                    ))
            && parsed.arguments.len() == 2
        {
            let key = parsed.arguments[0].to_string();
            let value = parsed.arguments[1].to_string();
            return match name {
                "Add" | "Set" | "ContainsKey" | "Remove" => Some("bool".to_owned()),
                "EnsureCapacity" => Some("int".to_owned()),
                "Clear" => Some("void".to_owned()),
                "GetOr" | "GetOrAdd" => Some(value),
                "Keys" => Some(format!("{key}[]")),
                "Values" => Some(format!("{value}[]")),
                _ => None,
            };
        }
        if parsed.array_depth == 0 && parsed.base == "Task" && parsed.arguments.len() == 1 {
            return match name {
                "Wait" => Some(parsed.arguments[0].to_string()),
                "Cancel" => Some("bool".to_owned()),
                _ => None,
            };
        }
        None
    }

    fn infer_readonly(
        &self,
        expression: &Expression,
        environment: &HashMap<String, String>,
    ) -> String {
        match &expression.kind {
            ExpressionKind::Literal(value) => literal_type(value),
            ExpressionKind::Name(name) => environment.get(name).cloned().unwrap_or_else(|| {
                if self.declarations.contains_key(name)
                    || self.methods.keys().any(|(owner, _)| owner == name)
                    || self.method_templates.keys().any(|(owner, _)| owner == name)
                    || self.is_enum_type_name(name)
                {
                    name.clone()
                } else {
                    String::new()
                }
            }),
            ExpressionKind::This => environment.get("this").cloned().unwrap_or_default(),
            ExpressionKind::Base => self.base_class_of(environment).unwrap_or_default(),
            ExpressionKind::NullConditionalReceiver => self
                .null_conditional_receiver_types
                .last()
                .cloned()
                .unwrap_or_default(),
            ExpressionKind::NewObject { type_name, .. } => type_name.clone().unwrap_or_default(),
            ExpressionKind::ObjectInitializer { creation, .. } => {
                self.infer_readonly(creation, environment)
            }
            ExpressionKind::StructLiteral { type_name, .. } => type_name.clone(),
            ExpressionKind::Member { object, name } => {
                if let ExpressionKind::Name(enum_name) = &object.kind
                    && !environment.contains_key(enum_name)
                    && self
                        .enum_case_fields(enum_name, name)
                        .is_some_and(|fields| fields.is_empty())
                {
                    return enum_name.clone();
                }
                let owner = self.infer_readonly(object, environment);
                self.member_result_type(&owner, name)
            }
            ExpressionKind::Index { array, .. } => {
                let owner = self.infer_readonly(array, environment);
                Self::index_result_type(&owner)
            }
            ExpressionKind::Call {
                callee, arguments, ..
            } => self.infer_call_readonly(callee, arguments, environment),
            ExpressionKind::Unary { operator, operand } => {
                let inner = self.infer_readonly(operand, environment);
                if *operator == aster_syntax::UnaryOperator::Not {
                    "bool".to_owned()
                } else {
                    inner
                }
            }
            ExpressionKind::IncrementDecrement { operand, .. } => {
                self.infer_readonly(operand, environment)
            }
            ExpressionKind::Try { operand } => {
                let inner = self.infer_readonly(operand, environment);
                Self::unwrap_try_name(&inner).unwrap_or(inner)
            }
            ExpressionKind::Await { operand } => {
                let inner = self.infer_readonly(operand, environment);
                Self::unwrap_task_name(&inner).unwrap_or(inner)
            }
            ExpressionKind::Cast { target, .. } => target.name.clone(),
            ExpressionKind::TypeAs { target, .. } => {
                crate::standard_library::option_specialization_name(&target.name)
            }
            ExpressionKind::Conditional {
                when_true,
                when_false,
                ..
            } => self.infer_conditional_readonly(when_true, when_false, environment),
            ExpressionKind::Switch {
                value,
                cases,
                default,
            } => self.infer_switch_readonly(value, cases, default.as_deref(), environment),
            ExpressionKind::NullCoalesce { value, fallback } => {
                let left = self.infer_readonly(value, environment);
                let right = self.infer_readonly(fallback, environment);
                Self::null_coalesce_result(&left, &right, fallback)
            }
            ExpressionKind::NullConditional { receiver, .. } => {
                let _ = receiver;
                String::new()
            }
            _ => String::new(),
        }
    }

    fn infer_conditional_readonly(
        &self,
        when_true: &Expression,
        when_false: &Expression,
        environment: &HashMap<String, String>,
    ) -> String {
        let first = self.infer_readonly(when_true, environment);
        let second = self.infer_readonly(when_false, environment);
        let first_is_null = matches!(
            when_true.kind,
            ExpressionKind::Literal(aster_syntax::Literal::Null)
        );
        let second_is_null = matches!(
            when_false.kind,
            ExpressionKind::Literal(aster_syntax::Literal::Null)
        );
        self.combine_branch_types(&first, first_is_null, &second, second_is_null)
            .0
    }

    fn infer_switch_readonly(
        &self,
        value: &Expression,
        cases: &[aster_syntax::SwitchExpressionCase],
        default: Option<&Expression>,
        environment: &HashMap<String, String>,
    ) -> String {
        let selected = self.infer_readonly(value, environment);
        let mut result: Option<(String, bool)> = None;
        for case in cases {
            let mut case_env = environment.clone();
            if let Some(fields) = self.enum_case_fields(&selected, &case.case_name) {
                for (binding, field_type) in case.bindings.iter().zip(fields) {
                    case_env.insert(binding.clone(), field_type);
                }
            }
            let arm = self.infer_readonly(&case.value, &case_env);
            let is_null = matches!(
                case.value.kind,
                ExpressionKind::Literal(aster_syntax::Literal::Null)
            );
            result = Some(match result {
                None => (arm, is_null),
                Some((current, current_is_null)) => {
                    self.combine_branch_types(&current, current_is_null, &arm, is_null)
                }
            });
        }
        if let Some(default) = default {
            let arm = self.infer_readonly(default, environment);
            let is_null = matches!(
                default.kind,
                ExpressionKind::Literal(aster_syntax::Literal::Null)
            );
            result = Some(match result {
                None => (arm, is_null),
                Some((current, current_is_null)) => {
                    self.combine_branch_types(&current, current_is_null, &arm, is_null)
                }
            });
        }
        result.map_or_else(String::new, |(type_, _)| type_)
    }

    fn infer_call_readonly(
        &self,
        callee: &Expression,
        arguments: &[aster_syntax::Argument],
        environment: &HashMap<String, String>,
    ) -> String {
        match &callee.kind {
            ExpressionKind::Name(name) => environment
                .get("#owner")
                .map(|owner| self.lookup_method_return(owner, name))
                .filter(|result| !result.is_empty())
                .or_else(|| self.returns.get(name).cloned())
                .unwrap_or_default(),
            ExpressionKind::Member { object, name } => {
                if let ExpressionKind::Name(enum_name) = &object.kind
                    && !environment.contains_key(enum_name)
                    && self.enum_case_fields(enum_name, name).is_some()
                {
                    return enum_name.clone();
                }
                let owner = self.infer_readonly(object, environment);
                if owner == "string"
                    && arguments.is_empty()
                    && let Some((_, target)) =
                        TRY_PARSE_TARGETS.iter().find(|(method, _)| method == name)
                {
                    return crate::standard_library::option_specialization_name(target);
                }
                if let Some(result) = Self::builtin_method_result(&owner, name) {
                    return result;
                }
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
                        return crate::standard_library::option_specialization_name(&value);
                    }
                    if name == "Entries" {
                        let entry_type =
                            crate::standard_library::dictionary_entry_specialization_name(
                                &key, &value,
                            );
                        return format!("{entry_type}[]");
                    }
                }
                self.lookup_method_return(&owner, name)
            }
            _ => String::new(),
        }
    }

    fn base_class_of(&self, environment: &HashMap<String, String>) -> Option<String> {
        let owner = environment.get("#owner")?;
        let bases = self.type_bases.get(owner)?;
        bases.iter().find_map(|base| {
            let base_key = TypeName::parse(base).map_or_else(|| base.clone(), |name| name.base);
            self.declarations
                .get(&base_key)
                .is_some_and(|facts| facts.kind == DeclarationKind::Class)
                .then(|| base.clone())
        })
    }

    fn next_bases(&self, current: &str) -> Vec<String> {
        if let Some(bases) = self.type_bases.get(current) {
            return bases.clone();
        }
        if let Some(parsed) = TypeName::parse(current)
            && !parsed.arguments.is_empty()
            && let Some(template) = self.type_templates.get(&parsed.base)
        {
            let declaration = template.declaration();
            if declaration.type_parameters.len() == parsed.arguments.len() {
                let parameter_names = declaration
                    .type_parameters
                    .iter()
                    .map(|parameter| parameter.name.clone())
                    .collect::<Vec<_>>();
                let concrete = parsed
                    .arguments
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                let substitutions = super::substitutions(&parameter_names, &concrete);
                return declaration
                    .interfaces
                    .iter()
                    .map(|base| super::substitute_name(&base.name, &substitutions))
                    .collect();
            }
        }
        Vec::new()
    }

    fn template_member_field_type(&self, current: &str, name: &str) -> Option<String> {
        let parsed = TypeName::parse(current)?;
        if parsed.arguments.is_empty() {
            return None;
        }
        let template = self.type_templates.get(&parsed.base)?;
        let declaration = template.declaration();
        if declaration.type_parameters.len() != parsed.arguments.len() {
            return None;
        }
        let parameter_names = declaration
            .type_parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let concrete = parsed
            .arguments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let substitutions = super::substitutions(&parameter_names, &concrete);
        declaration.members.iter().find_map(|member| match member {
            Member::Field(field) if field.name == name => {
                Some(super::substitute_name(&field.type_ref.name, &substitutions))
            }
            Member::Property(property) if property.name == name => Some(super::substitute_name(
                &property.type_ref.name,
                &substitutions,
            )),
            _ => None,
        })
    }

    fn template_member_method_return(&self, current: &str, name: &str) -> Option<String> {
        let parsed = TypeName::parse(current)?;
        if parsed.arguments.is_empty() {
            return None;
        }
        let template = self.type_templates.get(&parsed.base)?;
        let declaration = template.declaration();
        if declaration.type_parameters.len() != parsed.arguments.len() {
            return None;
        }
        let parameter_names = declaration
            .type_parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let concrete = parsed
            .arguments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let substitutions = super::substitutions(&parameter_names, &concrete);
        declaration.members.iter().find_map(|member| match member {
            Member::Method(method)
                if method.name == name
                    && !method.constructor
                    && method.type_parameters.is_empty() =>
            {
                Some(super::substitute_name(
                    &method.return_type.name,
                    &substitutions,
                ))
            }
            _ => None,
        })
    }

    pub(super) fn collect_owner_fields(&self, owner: &str) -> HashMap<String, String> {
        let mut fields = HashMap::new();
        if owner.is_empty() {
            return fields;
        }
        let mut queue = VecDeque::from([owner.to_owned()]);
        let mut visited = HashSet::new();
        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            for ((field_owner, field_name), field_type) in &self.fields {
                if field_owner == &current {
                    fields
                        .entry(field_name.clone())
                        .or_insert_with(|| field_type.clone());
                }
            }
            for base in self.next_bases(&current) {
                if !visited.contains(&base) {
                    queue.push_back(base);
                }
            }
        }
        fields
    }

    fn lookup_field_type(&self, owner: &str, name: &str) -> String {
        if owner.is_empty() {
            return String::new();
        }
        let mut queue = VecDeque::from([owner.to_owned()]);
        let mut visited = HashSet::new();
        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(type_) = self.fields.get(&(current.clone(), name.to_owned())) {
                return type_.clone();
            }
            if let Some(type_) = self.template_member_field_type(&current, name) {
                return type_;
            }
            for base in self.next_bases(&current) {
                if !visited.contains(&base) {
                    queue.push_back(base);
                }
            }
        }
        String::new()
    }

    fn lookup_method_return(&self, owner: &str, name: &str) -> String {
        if owner.is_empty() {
            return String::new();
        }
        let mut queue = VecDeque::from([owner.to_owned()]);
        let mut visited = HashSet::new();
        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(type_) = self.methods.get(&(current.clone(), name.to_owned())) {
                return type_.clone();
            }
            if let Some(type_) = self.template_member_method_return(&current, name) {
                return type_;
            }
            for base in self.next_bases(&current) {
                if !visited.contains(&base) {
                    queue.push_back(base);
                }
            }
        }
        String::new()
    }

    fn resolve_method_templates(
        &self,
        owner: &str,
        name: &str,
    ) -> Option<(String, Vec<FunctionDeclaration>)> {
        if owner.is_empty() {
            return None;
        }
        let mut queue = VecDeque::from([owner.to_owned()]);
        let mut visited = HashSet::new();
        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(templates) = self
                .method_templates
                .get(&(current.clone(), name.to_owned()))
            {
                return Some((current, templates.clone()));
            }
            for base in self.next_bases(&current) {
                if !visited.contains(&base) {
                    queue.push_back(base);
                }
            }
        }
        None
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
