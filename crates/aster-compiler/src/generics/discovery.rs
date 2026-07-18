use super::{
    BinaryOperator, Block, Diagnostic, Expression, ExpressionKind, FunctionDeclaration, HashMap,
    Member, Monomorphizer, Statement, TypeDeclaration, TypeRef, literal_type, variable_type,
};

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
}
