use super::types::convert;
use super::{HashMap, Lowerer, ast, hir};
use crate::constexpr::evaluate;

impl Lowerer<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn predeclare(&mut self, module: &ast::Module) {
        for item in &module.items {
            match item {
                ast::Item::Class(item) => {
                    let symbol = self.allocate();
                    self.types.insert(item.name.clone(), symbol);
                    self.item_symbols.insert(item.name.clone(), symbol);
                    self.globals.insert(item.name.clone(), symbol);
                    self.class_types.insert(symbol);
                    self.symbol_types.insert(symbol, hir::Type::Class(symbol));
                }
                ast::Item::Struct(item) => {
                    let symbol = self.allocate();
                    self.types.insert(item.name.clone(), symbol);
                    self.item_symbols.insert(item.name.clone(), symbol);
                    self.globals.insert(item.name.clone(), symbol);
                    self.symbol_types.insert(symbol, hir::Type::User(symbol));
                }
                ast::Item::Interface(item) => {
                    let symbol = self.allocate();
                    self.types.insert(item.name.clone(), symbol);
                    self.item_symbols.insert(item.name.clone(), symbol);
                    self.globals.insert(item.name.clone(), symbol);
                    self.interface_types.insert(symbol);
                    self.symbol_types
                        .insert(symbol, hir::Type::Interface(symbol));
                }
                ast::Item::Enum(item) => {
                    let symbol = self.allocate();
                    self.types.insert(item.name.clone(), symbol);
                    self.item_symbols.insert(item.name.clone(), symbol);
                    self.globals.insert(item.name.clone(), symbol);
                    self.enum_types.insert(symbol);
                    self.symbol_types.insert(symbol, hir::Type::Enum(symbol));
                }
                _ => {}
            }
        }
        for item in &module.items {
            match item {
                ast::Item::Function(function) => {
                    let symbol = self.allocate();
                    self.callable_symbols.insert(
                        crate::semantic::callable_key(
                            &function.name,
                            function.span.start,
                            None,
                            None,
                        ),
                        symbol,
                    );
                    self.item_symbols
                        .entry(function.name.clone())
                        .or_insert(symbol);
                    self.globals.entry(function.name.clone()).or_insert(symbol);
                    let result = self.resolve_type(&function.return_type);
                    self.callable_results.insert(symbol, result);
                    let parameters = function
                        .parameters
                        .iter()
                        .map(|parameter| self.resolve_type(&parameter.type_ref))
                        .collect();
                    self.callable_parameters.insert(symbol, parameters);
                }
                ast::Item::Variable(variable) => {
                    let symbol = self.allocate();
                    self.item_symbols.insert(variable.name.clone(), symbol);
                    self.globals.insert(variable.name.clone(), symbol);
                    let type_ = self.variable_declared_type(variable);
                    self.symbol_types.insert(symbol, type_);
                    // Evaluate module-level constants before any body lowers,
                    // so functions declared earlier in the file still fold
                    // references to them.
                    if let ast::VariableKind::Constant(type_ref) = &variable.kind
                        && let Some(initializer) = &variable.initializer
                    {
                        let resolve = |name: &str| {
                            self.item_symbols
                                .get(name)
                                .and_then(|symbol| self.constant_values.get(symbol).cloned())
                        };
                        if let Ok(value) = evaluate(initializer, &resolve) {
                            self.constant_values
                                .insert(symbol, value.coerce_to(&type_ref.name));
                        }
                    }
                }
                ast::Item::Class(declaration)
                | ast::Item::Struct(declaration)
                | ast::Item::Interface(declaration) => self.predeclare_members(declaration),
                ast::Item::Enum(declaration) => {
                    for (index, case) in declaration.cases.iter().enumerate() {
                        let case_symbol = self.allocate();
                        let mut fields = Vec::new();
                        for field in &case.fields {
                            let symbol = self.allocate();
                            self.symbol_types
                                .insert(symbol, self.resolve_type(&field.type_ref));
                            fields.push(symbol);
                        }
                        self.enum_cases
                            .insert((declaration.name.clone(), index), (case_symbol, fields));
                    }
                }
            }
        }
    }

    fn predeclare_members(&mut self, declaration: &ast::TypeDeclaration) {
        let owner = self.types[&declaration.name];
        let mut members = HashMap::new();
        for member in &declaration.members {
            if let ast::Member::Property(property) = member {
                if property.getter.is_some() {
                    self.predeclare_accessor(
                        owner,
                        &declaration.name,
                        property,
                        crate::semantic::AccessorKind::Get,
                    );
                }
                if property.setter.is_some() {
                    self.predeclare_accessor(
                        owner,
                        &declaration.name,
                        property,
                        crate::semantic::AccessorKind::Set,
                    );
                }
                continue;
            }
            let (name, type_, callable) = match member {
                ast::Member::Field(field) => (
                    field.name.as_str(),
                    self.resolve_type(&field.type_ref),
                    false,
                ),
                ast::Member::Method(method) => (
                    if method.constructor {
                        "#ctor"
                    } else {
                        method.name.as_str()
                    },
                    self.resolve_type(&method.return_type),
                    true,
                ),
                ast::Member::Property(_) => unreachable!(),
            };
            let symbol = self.allocate();
            members.entry(name.to_owned()).or_insert(symbol);
            self.member_owners.insert(symbol, owner);
            self.member_symbols
                .insert((declaration.name.clone(), name.to_owned()), symbol);
            if callable {
                if let ast::Member::Method(method) = member {
                    self.callable_symbols.insert(
                        crate::semantic::callable_key(
                            &method.name,
                            method.span.start,
                            None,
                            Some(&declaration.name),
                        ),
                        symbol,
                    );
                }
                self.callable_results.insert(symbol, type_);
                if let ast::Member::Method(method) = member {
                    let parameters = method
                        .parameters
                        .iter()
                        .map(|parameter| self.resolve_type(&parameter.type_ref))
                        .collect();
                    self.callable_parameters.insert(symbol, parameters);
                }
            } else {
                self.symbol_types.insert(symbol, type_);
            }
        }
        self.members.insert(owner, members);
    }

    fn predeclare_accessor(
        &mut self,
        owner: hir::SymbolId,
        owner_name: &str,
        property: &ast::Property,
        kind: crate::semantic::AccessorKind,
    ) {
        let symbol = self.allocate();
        self.member_owners.insert(symbol, owner);
        self.callable_symbols.insert(
            crate::semantic::callable_key(
                &property.name,
                property.span.start,
                Some(kind),
                Some(owner_name),
            ),
            symbol,
        );
        let property_type = self.resolve_type(&property.type_ref);
        let (result, parameters) = match kind {
            crate::semantic::AccessorKind::Get => (property_type, Vec::new()),
            crate::semantic::AccessorKind::Set => (hir::Type::Void, vec![property_type]),
        };
        self.callable_results.insert(symbol, result);
        self.callable_parameters.insert(symbol, parameters);
    }

    pub(super) fn enum_declaration(
        &self,
        declaration: &ast::EnumDeclaration,
    ) -> hir::EnumDeclaration {
        let symbol = self.item_symbols[&declaration.name];
        let cases = declaration
            .cases
            .iter()
            .enumerate()
            .map(|(index, case)| {
                let (case_symbol, field_symbols) =
                    &self.enum_cases[&(declaration.name.clone(), index)];
                hir::EnumCase {
                    symbol: *case_symbol,
                    name: case.name.clone(),
                    tag: u32::try_from(index).expect("enum case count validated"),
                    fields: case
                        .fields
                        .iter()
                        .zip(field_symbols)
                        .map(|(field, symbol)| hir::Field {
                            symbol: *symbol,
                            name: field.name.clone(),
                            visibility: hir::Visibility::Public,
                            type_: self.resolve_type(&field.type_ref),
                            initializer: None,
                        })
                        .collect(),
                }
            })
            .collect();
        hir::EnumDeclaration {
            symbol,
            name: declaration.name.clone(),
            visibility: visibility(declaration.visibility),
            cases,
        }
    }

    pub(super) fn type_declaration(
        &mut self,
        declaration: &ast::TypeDeclaration,
    ) -> hir::TypeDeclaration {
        let owner_symbol = self.item_symbols[&declaration.name];
        let owner_members = self.members.get(&owner_symbol).cloned().unwrap_or_default();
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        for member in &declaration.members {
            match member {
                ast::Member::Field(field) => {
                    let symbol = owner_members[&field.name];
                    let initializer = field
                        .initializer
                        .as_ref()
                        .map(|value| self.field_initializer(declaration, field, value));
                    fields.push(hir::Field {
                        symbol,
                        name: field.name.clone(),
                        visibility: visibility(field.visibility),
                        type_: self.resolve_type(&field.type_ref),
                        initializer,
                    });
                }
                ast::Member::Method(method) => {
                    methods.push(self.function(method, Some(declaration), None));
                }
                ast::Member::Property(property) => {
                    if let Some(getter) = &property.getter {
                        let function = accessor_function(property, getter, true);
                        methods.push(self.function(
                            &function,
                            Some(declaration),
                            Some(crate::semantic::AccessorKind::Get),
                        ));
                    }
                    if let Some(setter) = &property.setter {
                        let function = accessor_function(property, setter, false);
                        methods.push(self.function(
                            &function,
                            Some(declaration),
                            Some(crate::semantic::AccessorKind::Set),
                        ));
                    }
                }
            }
        }
        hir::TypeDeclaration {
            symbol: owner_symbol,
            name: declaration.name.clone(),
            visibility: visibility(declaration.visibility),
            interfaces: declaration
                .interfaces
                .iter()
                .filter_map(|interface| self.types.get(&interface.name).copied())
                .collect(),
            fields,
            methods,
        }
    }

    pub(super) fn function(
        &mut self,
        function: &ast::FunctionDeclaration,
        owner: Option<&ast::TypeDeclaration>,
        accessor: Option<crate::semantic::AccessorKind>,
    ) -> hir::Function {
        let previous_model_context = std::mem::replace(
            &mut self.model_context,
            crate::semantic::function_context(function, owner),
        );
        let symbol = self.callable_symbols[&crate::semantic::callable_key(
            &function.name,
            function.span.start,
            accessor,
            owner.map(|owner| owner.name.as_str()),
        )];
        let mut scope = HashMap::new();
        let previous_receiver = self.current_receiver;
        let mut receiver = None;
        if let Some(owner) = owner {
            let owner_symbol = self.types[&owner.name];
            scope.extend(self.members.get(&owner_symbol).cloned().unwrap_or_default());
            if self.class_types.contains(&owner_symbol) && !function.is_static {
                let receiver_symbol = self.allocate();
                self.symbol_types
                    .insert(receiver_symbol, hir::Type::Class(owner_symbol));
                scope.insert("this".to_owned(), receiver_symbol);
                self.current_receiver = Some(receiver_symbol);
                receiver = Some(hir::Parameter {
                    symbol: receiver_symbol,
                    name: "this".to_owned(),
                    type_: hir::Type::Class(owner_symbol),
                });
            }
        }
        let mut parameters = function
            .parameters
            .iter()
            .map(|parameter| {
                let parameter_symbol = self.allocate();
                let type_ = self.resolve_type(&parameter.type_ref);
                self.symbol_types.insert(parameter_symbol, type_.clone());
                scope.insert(parameter.name.clone(), parameter_symbol);
                hir::Parameter {
                    symbol: parameter_symbol,
                    name: parameter.name.clone(),
                    type_,
                }
            })
            .collect::<Vec<_>>();
        if let Some(receiver) = receiver {
            parameters.insert(0, receiver);
        }
        let return_type = self.resolve_type(&function.return_type);
        // Inside an `async Task<T>` body, `return` yields `T`, not `Task<T>`
        // (the wrapper produces the `Task<T>` handle). Checking and converting
        // returns against `T` keeps the awaited/returned values scalar so the
        // generated `MoveNext` publishes a scalar candidate result.
        let body_return = match &return_type {
            hir::Type::Task(inner) if function.is_async => (**inner).clone(),
            _ => return_type.clone(),
        };
        let previous_return = std::mem::replace(&mut self.current_return, body_return);
        self.scopes.push(scope);
        let mut body = function.body.as_ref().map(|body| self.block(body));
        if function.constructor
            && let (Some(owner), Some(receiver), Some(body)) =
                (owner, self.current_receiver, body.as_mut())
        {
            self.prepend_field_initializers(owner, receiver, body);
        }
        self.scopes.pop();
        self.current_return = previous_return;
        self.current_receiver = previous_receiver;
        self.model_context = previous_model_context;
        hir::Function {
            constructor: function.constructor,
            is_static: function.is_static,
            is_async: function.is_async,
            symbol,
            name: function.name.clone(),
            visibility: visibility(function.visibility),
            intrinsic: self.intrinsic_bindings.get(&function.name).copied(),
            parameters,
            return_type,
            body,
        }
    }

    fn field_initializer(
        &mut self,
        owner: &ast::TypeDeclaration,
        field: &ast::Field,
        initializer: &ast::Expression,
    ) -> hir::Expression {
        let previous_context = std::mem::replace(
            &mut self.model_context,
            crate::semantic::field_context(&owner.name, &field.name, field.span.start),
        );
        let initializer = self.expression(initializer);
        self.model_context = previous_context;
        initializer
    }

    fn prepend_field_initializers(
        &mut self,
        owner: &ast::TypeDeclaration,
        receiver: hir::SymbolId,
        body: &mut hir::Block,
    ) {
        let owner_symbol = self.types[&owner.name];
        let receiver_expression = hir::Expression {
            type_: hir::Type::Class(owner_symbol),
            kind: hir::ExpressionKind::Symbol(receiver),
        };
        let mut initializers = owner
            .members
            .iter()
            .filter_map(|member| {
                let ast::Member::Field(field) = member else {
                    return None;
                };
                let initializer = field.initializer.as_ref()?;
                let field_symbol = self.members[&owner_symbol][&field.name];
                let field_type = self.symbol_types[&field_symbol].clone();
                let initializer = self.field_initializer(owner, field, initializer);
                Some(hir::Statement::Expression(hir::Expression {
                    type_: field_type.clone(),
                    kind: hir::ExpressionKind::Assignment {
                        target: Box::new(hir::Expression {
                            type_: field_type.clone(),
                            kind: hir::ExpressionKind::Member {
                                object: Box::new(receiver_expression.clone()),
                                symbol: field_symbol,
                            },
                        }),
                        operator: hir::AssignmentOperator::Assign,
                        value: Box::new(convert(initializer, &field_type)),
                    },
                }))
            })
            .collect::<Vec<_>>();
        initializers.append(&mut body.statements);
        body.statements = initializers;
    }
}

fn accessor_function(
    property: &ast::Property,
    accessor: &ast::Accessor,
    getter: bool,
) -> ast::FunctionDeclaration {
    ast::FunctionDeclaration {
        constructor: false,
        is_static: false,
        is_async: false,
        type_parameters: Vec::new(),
        visibility: accessor.visibility,
        return_type: if getter {
            property.type_ref.clone()
        } else {
            ast::TypeRef::new("void", property.type_ref.span)
        },
        name: property.name.clone(),
        parameters: if getter {
            Vec::new()
        } else {
            vec![ast::Parameter {
                type_ref: property.type_ref.clone(),
                name: "value".to_owned(),
                span: accessor.span,
            }]
        },
        body: Some(accessor.body.clone()),
        span: property.span,
    }
}

pub(super) fn visibility(value: ast::Visibility) -> hir::Visibility {
    match value {
        ast::Visibility::Public => hir::Visibility::Public,
        ast::Visibility::Internal => hir::Visibility::Internal,
        ast::Visibility::Protected => hir::Visibility::Protected,
        ast::Visibility::Private => hir::Visibility::Private,
    }
}
