use std::collections::HashMap;
use std::collections::HashSet;

use aster_hir as hir;
use aster_syntax as ast;

use crate::constexpr::{ConstValue, evaluate};
use crate::primitives::{self, IntegerFit, UnsignedFit, classify_integer, classify_unsigned};

pub(crate) fn lower(
    module: &ast::Module,
    model: &crate::semantic::Model,
    intrinsic_bindings: &HashMap<String, hir::Intrinsic>,
) -> hir::Module {
    Lowerer::new(module, model, intrinsic_bindings).module(module)
}

struct Lowerer<'a> {
    model: &'a crate::semantic::Model,
    intrinsic_bindings: &'a HashMap<String, hir::Intrinsic>,
    next_symbol: u32,
    globals: HashMap<String, hir::SymbolId>,
    scopes: Vec<HashMap<String, hir::SymbolId>>,
    types: HashMap<String, hir::SymbolId>,
    class_types: HashSet<hir::SymbolId>,
    interface_types: HashSet<hir::SymbolId>,
    enum_types: HashSet<hir::SymbolId>,
    enum_cases: HashMap<(String, usize), (hir::SymbolId, Vec<hir::SymbolId>)>,
    member_owners: HashMap<hir::SymbolId, hir::SymbolId>,
    current_receiver: Option<hir::SymbolId>,
    symbol_types: HashMap<hir::SymbolId, hir::Type>,
    callable_results: HashMap<hir::SymbolId, hir::Type>,
    callable_parameters: HashMap<hir::SymbolId, Vec<hir::Type>>,
    current_return: hir::Type,
    members: HashMap<hir::SymbolId, HashMap<String, hir::SymbolId>>,
    item_symbols: HashMap<String, hir::SymbolId>,
    member_symbols: HashMap<(String, String), hir::SymbolId>,
    callable_symbols: HashMap<crate::semantic::CallableKey, hir::SymbolId>,
    /// Evaluated values of `const` declarations, used to fold constant
    /// references into literals during lowering.
    constant_values: HashMap<hir::SymbolId, ConstValue>,
    model_context: String,
}

impl<'a> Lowerer<'a> {
    fn new(
        module: &ast::Module,
        model: &'a crate::semantic::Model,
        intrinsic_bindings: &'a HashMap<String, hir::Intrinsic>,
    ) -> Self {
        let mut lowerer = Self {
            model,
            intrinsic_bindings,
            next_symbol: 0,
            globals: HashMap::new(),
            scopes: Vec::new(),
            types: HashMap::new(),
            class_types: HashSet::new(),
            interface_types: HashSet::new(),
            enum_types: HashSet::new(),
            enum_cases: HashMap::new(),
            member_owners: HashMap::new(),
            current_receiver: None,
            symbol_types: HashMap::new(),
            callable_results: HashMap::new(),
            callable_parameters: HashMap::new(),
            current_return: hir::Type::Void,
            members: HashMap::new(),
            item_symbols: HashMap::new(),
            member_symbols: HashMap::new(),
            callable_symbols: HashMap::new(),
            constant_values: HashMap::new(),
            model_context: String::new(),
        };
        lowerer.predeclare(module);
        lowerer
    }

    #[allow(clippy::too_many_lines)]
    fn predeclare(&mut self, module: &ast::Module) {
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

    fn module(mut self, module: &ast::Module) -> hir::Module {
        self.scopes.push(self.globals.clone());
        let mut items = Vec::new();
        for item in &module.items {
            match item {
                ast::Item::Class(item) => items.push(hir::Item::Class(self.type_declaration(item))),
                ast::Item::Struct(item) => {
                    items.push(hir::Item::Struct(self.type_declaration(item)));
                }
                ast::Item::Interface(item) => {
                    items.push(hir::Item::Interface(self.type_declaration(item)));
                }
                ast::Item::Enum(item) => {
                    items.push(hir::Item::Enum(self.enum_declaration(item)));
                }
                ast::Item::Function(item) => {
                    items.push(hir::Item::Function(self.function(item, None, None)));
                }
                ast::Item::Variable(item) => {
                    let symbol = self.item_symbols[&item.name];
                    items.push(hir::Item::Variable(self.variable(item, symbol)));
                }
            }
        }
        hir::Module { items }
    }

    fn enum_declaration(&self, declaration: &ast::EnumDeclaration) -> hir::EnumDeclaration {
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

    fn type_declaration(&mut self, declaration: &ast::TypeDeclaration) -> hir::TypeDeclaration {
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
                        .map(|value| self.expression(value));
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

    fn function(
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
        let previous_return = std::mem::replace(&mut self.current_return, return_type.clone());
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
            symbol,
            name: function.name.clone(),
            visibility: visibility(function.visibility),
            intrinsic: self.intrinsic_bindings.get(&function.name).copied(),
            parameters,
            return_type,
            body,
        }
    }

    fn block(&mut self, block: &ast::Block) -> hir::Block {
        self.scopes.push(HashMap::new());
        let statements = block
            .statements
            .iter()
            .filter_map(|statement| self.statement(statement))
            .collect();
        self.scopes.pop();
        hir::Block { statements }
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

    #[allow(clippy::too_many_lines)]
    fn statement(&mut self, statement: &ast::Statement) -> Option<hir::Statement> {
        Some(match statement {
            ast::Statement::Variable(variable) => {
                let symbol = self.allocate();
                let variable = self.variable(variable, symbol);
                self.scopes
                    .last_mut()?
                    .insert(variable.name.clone(), symbol);
                hir::Statement::Variable(variable)
            }
            ast::Statement::Return { value, .. } => {
                let target = self.current_return.clone();
                hir::Statement::Return(
                    value
                        .as_ref()
                        .map(|value| convert(self.expression(value), &target)),
                )
            }
            ast::Statement::Expression(expression) => {
                hir::Statement::Expression(self.expression(expression))
            }
            ast::Statement::If {
                condition,
                then_block,
                else_block,
                ..
            } => hir::Statement::If {
                condition: self.expression(condition),
                then_block: self.block(then_block),
                else_block: else_block.as_ref().map(|block| self.block(block)),
            },
            ast::Statement::While {
                condition, body, ..
            } => hir::Statement::While {
                condition: self.expression(condition),
                body: self.block(body),
            },
            ast::Statement::For {
                initializer,
                condition,
                update,
                body,
                ..
            } => {
                self.scopes.push(HashMap::new());
                let initializer = initializer
                    .as_ref()
                    .and_then(|value| self.statement(value))
                    .map(Box::new);
                let condition = condition.as_ref().map(|value| self.expression(value));
                let update = update.as_ref().map(|value| self.expression(value));
                let body = self.block(body);
                self.scopes.pop();
                hir::Statement::For {
                    initializer,
                    condition,
                    update,
                    body,
                }
            }
            ast::Statement::Switch {
                value,
                cases,
                default,
                ..
            } => {
                let value = self.expression(value);
                let mut lowered_cases = Vec::new();
                for case in cases {
                    let key = crate::semantic::ModelNodeKey {
                        context: self.model_context.clone(),
                        span: case.span,
                    };
                    let resolved = &self.model.switch_cases[&key];
                    let (case_symbol, field_symbols) =
                        self.enum_cases[&(resolved.enum_name.clone(), resolved.case_index)].clone();
                    self.scopes.push(HashMap::new());
                    let bindings = case
                        .bindings
                        .iter()
                        .zip(field_symbols)
                        .map(|(name, field)| {
                            let symbol = self.allocate();
                            let type_ = self.symbol_types[&field].clone();
                            self.symbol_types.insert(symbol, type_.clone());
                            self.scopes
                                .last_mut()
                                .expect("switch case scope")
                                .insert(name.clone(), symbol);
                            hir::Parameter {
                                symbol,
                                name: name.clone(),
                                type_,
                            }
                        })
                        .collect();
                    let body = self.block(&case.body);
                    self.scopes.pop();
                    lowered_cases.push(hir::SwitchCase {
                        case: case_symbol,
                        tag: u32::try_from(resolved.case_index).expect("validated enum tag"),
                        bindings,
                        body,
                    });
                }
                hir::Statement::Switch {
                    value,
                    cases: lowered_cases,
                    default: default.as_ref().map(|block| self.block(block)),
                }
            }
            ast::Statement::Break(_) => hir::Statement::Break,
            ast::Statement::Continue(_) => hir::Statement::Continue,
        })
    }

    fn variable(
        &mut self,
        variable: &ast::VariableDeclaration,
        symbol: hir::SymbolId,
    ) -> hir::Variable {
        if let ast::VariableKind::Constant(type_ref) = &variable.kind
            && let Some(initializer) = &variable.initializer
        {
            let folded = {
                let resolve = |name: &str| {
                    self.lookup(name)
                        .and_then(|symbol| self.constant_values.get(&symbol).cloned())
                };
                evaluate(initializer, &resolve).ok()
            };
            if let Some(value) = folded {
                let value = value.coerce_to(&type_ref.name);
                let type_ = self.resolve_type(type_ref);
                self.constant_values.insert(symbol, value.clone());
                self.symbol_types.insert(symbol, type_.clone());
                return hir::Variable {
                    symbol,
                    name: variable.name.clone(),
                    visibility: variable.visibility.map(visibility),
                    type_,
                    mutable: false,
                    initializer: Some(constant_expression(&value)),
                };
            }
        }
        let mut initializer = variable
            .initializer
            .as_ref()
            .map(|value| self.expression(value));
        let type_ = match &variable.kind {
            ast::VariableKind::Inferred => initializer
                .as_ref()
                .map_or(hir::Type::Unknown, |value| value.type_.clone()),
            ast::VariableKind::Explicit(type_ref) | ast::VariableKind::Constant(type_ref) => {
                self.resolve_type(type_ref)
            }
        };
        initializer = initializer.map(|value| convert(value, &type_));
        self.symbol_types.insert(symbol, type_.clone());
        hir::Variable {
            symbol,
            name: variable.name.clone(),
            visibility: variable.visibility.map(visibility),
            type_,
            mutable: !matches!(variable.kind, ast::VariableKind::Constant(_)),
            initializer,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn expression(&mut self, expression: &ast::Expression) -> hir::Expression {
        let model_key = crate::semantic::ModelNodeKey {
            context: self.model_context.clone(),
            span: expression.span,
        };
        if let Some(resolved) = self.model.enum_values.get(&model_key).cloned() {
            let enum_symbol = self.types[&resolved.enum_name];
            let (case_symbol, field_symbols) =
                self.enum_cases[&(resolved.enum_name, resolved.case_index)].clone();
            let arguments: &[ast::Expression] = match &expression.kind {
                ast::ExpressionKind::Call { arguments, .. } => arguments,
                ast::ExpressionKind::Member { .. } => &[],
                _ => unreachable!("enum case metadata belongs to a case expression"),
            };
            let fields = arguments
                .iter()
                .zip(field_symbols)
                .map(|(argument, field)| {
                    let target = self.symbol_types[&field].clone();
                    hir::FieldValue {
                        field,
                        value: convert(self.expression(argument), &target),
                    }
                })
                .collect();
            return hir::Expression {
                type_: hir::Type::Enum(enum_symbol),
                kind: hir::ExpressionKind::EnumValue {
                    enum_symbol,
                    case: case_symbol,
                    tag: u32::try_from(resolved.case_index).expect("validated enum tag"),
                    fields,
                },
            };
        }
        match &expression.kind {
            ast::ExpressionKind::Literal(literal) => {
                let (literal, type_) = literal_value(literal);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Literal(literal),
                }
            }
            ast::ExpressionKind::Name(name) => {
                if let Some(key) = self.model.property_reads.get(&model_key) {
                    let getter = self.callable_symbols[key];
                    let receiver = self
                        .current_receiver
                        .expect("validated property read has a receiver");
                    return self.property_get(
                        hir::Expression {
                            type_: self.symbol_types[&receiver].clone(),
                            kind: hir::ExpressionKind::Symbol(receiver),
                        },
                        getter,
                    );
                }
                let symbol = self
                    .lookup(name)
                    .expect("validated names resolve to a declaration");
                if let Some(value) = self.constant_values.get(&symbol) {
                    return constant_expression(value);
                }
                if self.member_owners.contains_key(&symbol)
                    && let Some(receiver) = self.current_receiver
                {
                    let object = hir::Expression {
                        type_: self.symbol_types[&receiver].clone(),
                        kind: hir::ExpressionKind::Symbol(receiver),
                    };
                    let type_ = self
                        .symbol_types
                        .get(&symbol)
                        .cloned()
                        .or_else(|| self.callable_results.get(&symbol).cloned())
                        .unwrap_or(hir::Type::Unknown);
                    return hir::Expression {
                        type_,
                        kind: hir::ExpressionKind::Member {
                            object: Box::new(object),
                            symbol,
                        },
                    };
                }
                let type_ = self
                    .symbol_types
                    .get(&symbol)
                    .cloned()
                    .unwrap_or(hir::Type::Unknown);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Symbol(symbol),
                }
            }
            ast::ExpressionKind::This => {
                let receiver = self
                    .current_receiver
                    .expect("validated this has a receiver");
                hir::Expression {
                    type_: self.symbol_types[&receiver].clone(),
                    kind: hir::ExpressionKind::Symbol(receiver),
                }
            }
            ast::ExpressionKind::StructLiteral { type_name, fields } => {
                let struct_symbol = self.types[type_name];
                let fields = fields
                    .iter()
                    .map(|field| {
                        let field_symbol = self.members[&struct_symbol][&field.name];
                        let target = self.symbol_types[&field_symbol].clone();
                        let value = convert(self.expression(&field.value), &target);
                        hir::FieldValue {
                            field: field_symbol,
                            value,
                        }
                    })
                    .collect();
                hir::Expression {
                    type_: hir::Type::User(struct_symbol),
                    kind: hir::ExpressionKind::StructLiteral {
                        struct_symbol,
                        fields,
                    },
                }
            }
            ast::ExpressionKind::ArrayLiteral(elements) => {
                let mut elements = elements
                    .iter()
                    .map(|value| self.expression(value))
                    .collect::<Vec<_>>();
                let element_type = elements
                    .first()
                    .map_or(hir::Type::Unknown, |value| value.type_.clone());
                for element in &mut elements {
                    *element = convert(element.clone(), &element_type);
                }
                hir::Expression {
                    type_: hir::Type::Array(Box::new(element_type)),
                    kind: hir::ExpressionKind::ArrayLiteral(elements),
                }
            }
            ast::ExpressionKind::NewArray {
                element_type,
                length,
            } => {
                let element_type = self.resolve_type(element_type);
                hir::Expression {
                    type_: hir::Type::Array(Box::new(element_type.clone())),
                    kind: hir::ExpressionKind::NewArray {
                        element_type,
                        length: Box::new(self.expression(length)),
                    },
                }
            }
            ast::ExpressionKind::NewObject {
                type_name,
                arguments,
            } => {
                let class_symbol = self.types[type_name];
                let constructor = self.callable_symbols[&self.model.constructors[&model_key]];
                let parameter_types = self.callable_parameters[&constructor].clone();
                let arguments = arguments
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        convert(self.expression(argument), &parameter_types[index])
                    })
                    .collect();
                hir::Expression {
                    type_: hir::Type::Class(class_symbol),
                    kind: hir::ExpressionKind::NewObject {
                        class_symbol,
                        constructor,
                        arguments,
                    },
                }
            }
            ast::ExpressionKind::Index { array, index } => {
                let array = self.expression(array);
                let element_type = match &array.type_ {
                    hir::Type::Array(element) => (**element).clone(),
                    _ => hir::Type::Unknown,
                };
                hir::Expression {
                    type_: element_type,
                    kind: hir::ExpressionKind::Index {
                        array: Box::new(array),
                        index: Box::new(self.expression(index)),
                    },
                }
            }
            ast::ExpressionKind::Member { object, name } => {
                let object = self.expression(object);
                if matches!(object.type_, hir::Type::Array(_)) {
                    return hir::Expression {
                        type_: hir::Type::Int,
                        kind: hir::ExpressionKind::ArrayLength(Box::new(object)),
                    };
                }
                if object.type_ == hir::Type::String {
                    return hir::Expression {
                        type_: hir::Type::Int,
                        kind: hir::ExpressionKind::StringLength(Box::new(object)),
                    };
                }
                if let Some(key) = self.model.property_reads.get(&model_key) {
                    return self.property_get(object, self.callable_symbols[key]);
                }
                let symbol = match object.type_ {
                    hir::Type::User(owner)
                    | hir::Type::Class(owner)
                    | hir::Type::Interface(owner) => self.members[&owner][name],
                    _ => unreachable!("validated member access has a user type"),
                };
                let type_ = self
                    .symbol_types
                    .get(&symbol)
                    .cloned()
                    .unwrap_or(hir::Type::Unknown);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Member {
                        object: Box::new(object),
                        symbol,
                    },
                }
            }
            ast::ExpressionKind::Call {
                callee, arguments, ..
            } => {
                if let Some(level) = log_level(callee) {
                    let argument = arguments
                        .first()
                        .expect("validated logging calls have one argument");
                    return hir::Expression {
                        type_: hir::Type::Void,
                        kind: hir::ExpressionKind::LogCall {
                            level,
                            argument: Box::new(self.expression(argument)),
                        },
                    };
                }
                let resolved = &self.model.calls[&model_key];
                let symbol = self.callable_symbols[&resolved.callable];
                let type_ = self.callable_results[&symbol].clone();
                let parameter_types = self.callable_parameters[&symbol].clone();
                let arguments = arguments
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        let mut argument = self.expression(value);
                        if let Some(target) = parameter_types.get(index) {
                            argument = convert(argument, target);
                        }
                        argument
                    })
                    .collect();
                let callee = match resolved.dispatch {
                    crate::semantic::Dispatch::Direct => hir::Expression {
                        type_: hir::Type::Unknown,
                        kind: hir::ExpressionKind::Symbol(symbol),
                    },
                    crate::semantic::Dispatch::Instance | crate::semantic::Dispatch::Interface => {
                        let receiver = match &callee.kind {
                            ast::ExpressionKind::Member { object, .. } => self.expression(object),
                            ast::ExpressionKind::Name(_) => {
                                let receiver = self
                                    .current_receiver
                                    .expect("validated instance call has a receiver");
                                hir::Expression {
                                    type_: self.symbol_types[&receiver].clone(),
                                    kind: hir::ExpressionKind::Symbol(receiver),
                                }
                            }
                            _ => unreachable!("validated method call has a member callee"),
                        };
                        hir::Expression {
                            type_: hir::Type::Unknown,
                            kind: hir::ExpressionKind::Member {
                                object: Box::new(receiver),
                                symbol,
                            },
                        }
                    }
                };
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Call {
                        callee: Box::new(callee),
                        arguments,
                    },
                }
            }
            ast::ExpressionKind::Unary { operator, operand } => {
                if *operator == ast::UnaryOperator::Negate
                    && matches!(
                        &operand.kind,
                        ast::ExpressionKind::Literal(ast::Literal::Integer(value))
                            if value == "9223372036854775808"
                    )
                {
                    return hir::Expression {
                        type_: hir::Type::Long,
                        kind: hir::ExpressionKind::Literal(hir::Literal::Integer(
                            "-9223372036854775808".to_owned(),
                        )),
                    };
                }
                let operand = self.expression(operand);
                let type_ = operand.type_.clone();
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Unary {
                        operator: unary(*operator),
                        operand: Box::new(operand),
                    },
                }
            }
            ast::ExpressionKind::IncrementDecrement {
                operator,
                prefix,
                operand,
            } => {
                let target = self.expression(operand);
                let type_ = target.type_.clone();
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::IncrementDecrement {
                        operator: increment(*operator),
                        prefix: *prefix,
                        target: Box::new(target),
                    },
                }
            }
            ast::ExpressionKind::Try { operand } => {
                let resolved = self.model.propagations[&model_key].clone();
                let value = self.expression(operand);
                let (ok_case, ok_fields) =
                    self.enum_cases[&(resolved.result_type.clone(), resolved.ok_index)].clone();
                let (error_case, error_fields) =
                    self.enum_cases[&(resolved.result_type.clone(), resolved.error_index)].clone();
                let (return_error_case, return_error_fields) = self.enum_cases[&(
                    resolved.function_result_type.clone(),
                    resolved.function_error_index,
                )]
                    .clone();
                let ok_field = ok_fields[0];
                let error_field = error_fields[0];
                let return_error_field = return_error_fields[0];
                let success_type = self.symbol_types[&ok_field].clone();
                let error_type = self.symbol_types[&error_field].clone();
                hir::Expression {
                    type_: success_type.clone(),
                    kind: hir::ExpressionKind::PropagateResult {
                        operand: Box::new(value),
                        success_type,
                        error_type,
                        ok_case,
                        ok_field,
                        ok_tag: tag(resolved.ok_index),
                        error_case,
                        error_field,
                        error_tag: tag(resolved.error_index),
                        return_type: self.current_return.clone(),
                        return_error_case,
                        return_error_field,
                        return_error_tag: tag(resolved.function_error_index),
                    },
                }
            }
            ast::ExpressionKind::Conditional {
                condition,
                when_true,
                when_false,
            } => {
                let condition = self.expression(condition);
                let when_true = self.expression(when_true);
                let when_false = self.expression(when_false);
                let type_ = conditional_type(&when_true.type_, &when_false.type_);
                let when_true = convert(when_true, &type_);
                let when_false = convert(when_false, &type_);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Conditional {
                        condition: Box::new(condition),
                        when_true: Box::new(when_true),
                        when_false: Box::new(when_false),
                    },
                }
            }
            ast::ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                let mut left = self.expression(left);
                let mut right = self.expression(right);
                if left.type_ != right.type_
                    && let Some(promoted) = promoted(&left.type_, &right.type_)
                {
                    left = convert(left, &promoted);
                    right = convert(right, &promoted);
                }
                let type_ = binary_type(*operator, &left.type_, &right.type_);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Binary {
                        left: Box::new(left),
                        operator: binary(*operator),
                        right: Box::new(right),
                    },
                }
            }
            ast::ExpressionKind::Assignment {
                target,
                operator,
                value,
            } => {
                if let Some(resolved) = self.model.property_assignments.get(&model_key) {
                    let object = match &target.kind {
                        ast::ExpressionKind::Member { object, .. } => self.expression(object),
                        ast::ExpressionKind::Name(_) => {
                            let receiver = self
                                .current_receiver
                                .expect("validated property assignment has a receiver");
                            hir::Expression {
                                type_: self.symbol_types[&receiver].clone(),
                                kind: hir::ExpressionKind::Symbol(receiver),
                            }
                        }
                        _ => unreachable!("property target is a name or member"),
                    };
                    let setter = self.callable_symbols[&resolved.setter];
                    let target_type = self.callable_parameters[&setter][0].clone();
                    let value = convert(self.expression(value), &target_type);
                    return hir::Expression {
                        type_: target_type,
                        kind: hir::ExpressionKind::PropertyAssignment {
                            object: Box::new(object),
                            getter: resolved
                                .getter
                                .as_ref()
                                .map(|key| self.callable_symbols[key]),
                            setter,
                            operator: assignment(*operator),
                            value: Box::new(value),
                        },
                    };
                }
                let target = self.expression(target);
                let type_ = target.type_.clone();
                let value = convert(self.expression(value), &type_);
                hir::Expression {
                    type_,
                    kind: hir::ExpressionKind::Assignment {
                        target: Box::new(target),
                        operator: assignment(*operator),
                        value: Box::new(value),
                    },
                }
            }
            ast::ExpressionKind::Cast { target, operand } => {
                let type_ = self.resolve_type(target);
                let operand = self.expression(operand);
                if operand.type_ == type_ {
                    operand
                } else {
                    hir::Expression {
                        type_,
                        kind: hir::ExpressionKind::Convert {
                            operand: Box::new(operand),
                        },
                    }
                }
            }
        }
    }

    fn property_get(&self, object: hir::Expression, getter: hir::SymbolId) -> hir::Expression {
        let type_ = self.callable_results[&getter].clone();
        hir::Expression {
            type_,
            kind: hir::ExpressionKind::Call {
                callee: Box::new(hir::Expression {
                    type_: hir::Type::Unknown,
                    kind: hir::ExpressionKind::Member {
                        object: Box::new(object),
                        symbol: getter,
                    },
                }),
                arguments: Vec::new(),
            },
        }
    }

    fn variable_declared_type(&self, variable: &ast::VariableDeclaration) -> hir::Type {
        match &variable.kind {
            ast::VariableKind::Explicit(type_ref) | ast::VariableKind::Constant(type_ref) => {
                self.resolve_type(type_ref)
            }
            ast::VariableKind::Inferred => hir::Type::Unknown,
        }
    }

    fn resolve_type(&self, type_ref: &ast::TypeRef) -> hir::Type {
        if let Some(element) = type_ref.name.strip_suffix("[]") {
            return hir::Type::Array(Box::new(
                self.resolve_type(&ast::TypeRef::new(element, type_ref.span)),
            ));
        }
        if type_ref.name == "void" {
            return hir::Type::Void;
        }
        if let Some(primitive) = primitives::from_name(&type_ref.name) {
            return primitives::to_hir(primitive);
        }
        self.types
            .get(type_ref.name.as_str())
            .copied()
            .map_or(hir::Type::Unknown, |symbol| {
                if self.class_types.contains(&symbol) {
                    hir::Type::Class(symbol)
                } else if self.interface_types.contains(&symbol) {
                    hir::Type::Interface(symbol)
                } else if self.enum_types.contains(&symbol) {
                    hir::Type::Enum(symbol)
                } else {
                    hir::Type::User(symbol)
                }
            })
    }

    fn lookup(&self, name: &str) -> Option<hir::SymbolId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn allocate(&mut self) -> hir::SymbolId {
        let symbol = hir::SymbolId(self.next_symbol);
        self.next_symbol += 1;
        symbol
    }
}

/// Recognize the validated standard-library logging surface.
fn log_level(callee: &ast::Expression) -> Option<hir::LogLevel> {
    match &callee.kind {
        ast::ExpressionKind::Name(name) if name == "Log" => Some(hir::LogLevel::Log),
        ast::ExpressionKind::Member { object, name } => match &object.kind {
            ast::ExpressionKind::Name(object) if object == "Log" => match name.as_str() {
                "Warning" => Some(hir::LogLevel::Warning),
                "Error" => Some(hir::LogLevel::Error),
                _ => None,
            },
            _ => None,
        },
        _ => None,
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

fn literal_value(literal: &ast::Literal) -> (hir::Literal, hir::Type) {
    match literal {
        ast::Literal::Integer(value) => {
            let type_ = match classify_integer(value) {
                Some(IntegerFit::Long) => hir::Type::Long,
                _ => hir::Type::Int,
            };
            (hir::Literal::Integer(value.clone()), type_)
        }
        ast::Literal::Long(value) => (hir::Literal::Integer(value.clone()), hir::Type::Long),
        ast::Literal::UInt(value) => {
            let type_ = match classify_unsigned(value) {
                Some(UnsignedFit::ULong) => hir::Type::ULong,
                _ => hir::Type::UInt,
            };
            (hir::Literal::Integer(value.clone()), type_)
        }
        ast::Literal::ULong(value) => (hir::Literal::Integer(value.clone()), hir::Type::ULong),
        ast::Literal::Float(value) => (hir::Literal::Float(value.clone()), hir::Type::Float),
        ast::Literal::Double(value) => (hir::Literal::Float(value.clone()), hir::Type::Double),
        ast::Literal::Decimal(value) => (hir::Literal::Decimal(value.clone()), hir::Type::Decimal),
        ast::Literal::String(value) => (hir::Literal::String(value.clone()), hir::Type::String),
        ast::Literal::Character(value) => (hir::Literal::Character(*value), hir::Type::Char),
        ast::Literal::Boolean(value) => (hir::Literal::Boolean(*value), hir::Type::Bool),
    }
}

/// Materialize an evaluated constant as a literal expression.
fn constant_expression(value: &ConstValue) -> hir::Expression {
    let (literal, type_) = match value {
        ConstValue::Integer(value, kind) => (
            hir::Literal::Integer(value.to_string()),
            primitives::to_hir(*kind),
        ),
        ConstValue::Float(value) => (hir::Literal::Float(value.to_string()), hir::Type::Float),
        ConstValue::Double(value) => (hir::Literal::Float(value.to_string()), hir::Type::Double),
        ConstValue::Decimal(value) => (hir::Literal::Decimal(value.clone()), hir::Type::Decimal),
        ConstValue::Bool(value) => (hir::Literal::Boolean(*value), hir::Type::Bool),
        ConstValue::Char(value) => (hir::Literal::Character(*value), hir::Type::Char),
        ConstValue::Str(value) => (hir::Literal::String(value.clone()), hir::Type::String),
    };
    hir::Expression {
        type_,
        kind: hir::ExpressionKind::Literal(literal),
    }
}

/// Wrap an expression in a `Convert` node when its type differs from the
/// validated target type. Only value types are ever converted.
fn convert(expression: hir::Expression, target: &hir::Type) -> hir::Expression {
    if let (hir::Type::Class(class), hir::Type::Interface(interface)) =
        (expression.type_.clone(), target)
    {
        return hir::Expression {
            type_: target.clone(),
            kind: hir::ExpressionKind::UpcastInterface {
                object: Box::new(expression),
                class,
                interface: *interface,
            },
        };
    }
    if &expression.type_ == target
        || matches!(
            expression.type_,
            hir::Type::Unknown | hir::Type::User(_) | hir::Type::Class(_) | hir::Type::Interface(_)
        )
        || matches!(
            target,
            hir::Type::Unknown
                | hir::Type::Void
                | hir::Type::User(_)
                | hir::Type::Class(_)
                | hir::Type::Interface(_)
        )
    {
        return expression;
    }
    hir::Expression {
        type_: target.clone(),
        kind: hir::ExpressionKind::Convert {
            operand: Box::new(expression),
        },
    }
}

/// The validated common type of two `?:` branches (the promotion table for
/// numeric branches, or their identical type otherwise).
fn conditional_type(left: &hir::Type, right: &hir::Type) -> hir::Type {
    if left == right {
        return left.clone();
    }
    match (left, right) {
        (hir::Type::Class(_), hir::Type::Interface(_)) => return right.clone(),
        (hir::Type::Interface(_), hir::Type::Class(_)) => return left.clone(),
        _ => {}
    }
    promoted(left, right).unwrap_or_else(|| left.clone())
}

/// The validated common numeric type of two operands, from the central table.
fn promoted(left: &hir::Type, right: &hir::Type) -> Option<hir::Type> {
    let (left, right) = (primitives::of_hir(left)?, primitives::of_hir(right)?);
    primitives::promote(left, right).map(primitives::to_hir)
}

fn binary_type(operator: ast::BinaryOperator, left: &hir::Type, right: &hir::Type) -> hir::Type {
    use ast::BinaryOperator::{
        Equal, Greater, GreaterEqual, Less, LessEqual, LogicalAnd, LogicalOr, NotEqual,
    };
    if matches!(
        operator,
        Equal | NotEqual | Less | LessEqual | Greater | GreaterEqual | LogicalAnd | LogicalOr
    ) {
        return hir::Type::Bool;
    }
    if left == right {
        return left.clone();
    }
    promoted(left, right).unwrap_or_else(|| left.clone())
}

fn visibility(value: ast::Visibility) -> hir::Visibility {
    match value {
        ast::Visibility::Public => hir::Visibility::Public,
        ast::Visibility::Internal => hir::Visibility::Internal,
        ast::Visibility::Protected => hir::Visibility::Protected,
        ast::Visibility::Private => hir::Visibility::Private,
    }
}

fn tag(index: usize) -> u32 {
    u32::try_from(index).expect("validated enum tag")
}

macro_rules! convert_operator {
    ($function:ident, $source:ty, $target:ty, [$($variant:ident),+ $(,)?]) => {
        fn $function(value: $source) -> $target {
            match value { $(<$source>::$variant => <$target>::$variant,)+ }
        }
    };
}

convert_operator!(unary, ast::UnaryOperator, hir::UnaryOperator, [Not, Negate]);
convert_operator!(
    increment,
    ast::IncrementOperator,
    hir::IncrementOperator,
    [Increment, Decrement]
);
convert_operator!(
    binary,
    ast::BinaryOperator,
    hir::BinaryOperator,
    [
        Multiply,
        Divide,
        Remainder,
        Add,
        Subtract,
        Less,
        LessEqual,
        Greater,
        GreaterEqual,
        Equal,
        NotEqual,
        LogicalAnd,
        LogicalOr
    ]
);
convert_operator!(
    assignment,
    ast::AssignmentOperator,
    hir::AssignmentOperator,
    [
        Assign,
        AddAssign,
        SubtractAssign,
        MultiplyAssign,
        DivideAssign
    ]
);
