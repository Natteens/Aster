use super::types::{constant_expression, convert};
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
            if let ast::Item::Class(declaration) = item
                && let Some(first) = declaration.interfaces.first()
                && let Some(base) = self.types.get(&first.name).copied()
                && self.class_types.contains(&base)
            {
                self.class_bases.insert(self.types[&declaration.name], base);
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
                            &function.parameters,
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
                        self.enum_cases.insert(
                            (declaration.name.clone(), index),
                            (case_symbol, fields.clone()),
                        );
                        self.enum_case_by_name.insert(
                            (declaration.name.clone(), case.name.clone()),
                            (case_symbol, fields),
                        );
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
                            &method.parameters,
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
                &[],
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
            base: self.class_bases.get(&owner_symbol).copied(),
            is_abstract: declaration.is_abstract,
            is_sealed: declaration.is_sealed,
            interfaces: declaration
                .interfaces
                .iter()
                .filter_map(|interface| self.types.get(&interface.name).copied())
                .filter(|symbol| self.interface_types.contains(symbol))
                .collect(),
            fields,
            methods,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn function(
        &mut self,
        function: &ast::FunctionDeclaration,
        owner: Option<&ast::TypeDeclaration>,
        accessor: Option<crate::semantic::AccessorKind>,
    ) -> hir::Function {
        let context = match (accessor, owner) {
            (Some(kind), Some(owner)) => {
                // A setter's property type is its `value` parameter.
                let property_type = function
                    .parameters
                    .first()
                    .map_or(&function.return_type.name, |parameter| {
                        &parameter.type_ref.name
                    });
                crate::semantic::accessor_context(&owner.name, &function.name, kind, property_type)
            }
            _ => crate::semantic::function_context(function, owner),
        };
        let previous_model_context = std::mem::replace(&mut self.model_context, context);
        let symbol = self.callable_symbols[&crate::semantic::callable_key(
            &function.name,
            function.span.start,
            if accessor.is_some() {
                &[]
            } else {
                &function.parameters
            },
            accessor,
            owner.map(|owner| owner.name.as_str()),
        )];
        let mut scope = HashMap::new();
        let previous_receiver = self.current_receiver;
        let mut receiver = None;
        if let Some(owner) = owner {
            let owner_symbol = self.types[&owner.name];
            // Inherited members first, so a member of the class itself wins.
            for class in self.class_chain(owner_symbol).into_iter().rev() {
                scope.extend(self.members.get(&class).cloned().unwrap_or_default());
            }
            if !self.interface_types.contains(&owner_symbol) && !function.is_static {
                let receiver_type = self.symbol_types[&owner_symbol].clone();
                let receiver_symbol = self.allocate();
                self.symbol_types
                    .insert(receiver_symbol, receiver_type.clone());
                scope.insert("this".to_owned(), receiver_symbol);
                self.current_receiver = Some(receiver_symbol);
                receiver = Some(hir::Parameter {
                    symbol: receiver_symbol,
                    name: "this".to_owned(),
                    type_: receiver_type,
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
        let dispatch = self.method_dispatch(function, owner, accessor);
        let mut body = if function.dispatch == aster_syntax::MemberDispatch::Abstract {
            None
        } else {
            function.body.as_ref().map(|body| self.block(body))
        };
        if function.constructor
            && let (Some(owner), Some(receiver), Some(body)) =
                (owner, self.current_receiver, body.as_mut())
        {
            self.prepend_constructor_prologue(owner, function, receiver, body);
        }
        self.scopes.pop();
        self.current_return = previous_return;
        self.current_receiver = previous_receiver;
        self.model_context = previous_model_context;
        let intrinsic = self
            .intrinsic_bindings
            .get(&function.name)
            .copied()
            .map(|intrinsic| self.resolve_intrinsic_payload(intrinsic, &return_type));
        hir::Function {
            dispatch,
            constructor: function.constructor,
            is_test: function.is_test,
            is_static: function.is_static,
            is_async: function.is_async,
            is_foreign: function.is_foreign,
            symbol,
            name: function.name.clone(),
            visibility: visibility(function.visibility),
            intrinsic,
            parameters,
            return_type,
            body,
        }
    }

    /// `StandardLibrary::intrinsic_bindings()` can only tag *which* intrinsic
    /// a function is bound to -- it runs before any symbol exists. Filesystem
    /// intrinsics carry a payload of concrete symbols (see
    /// `hir::FileIoResultLayout`) that only HIR lowering can resolve, since it
    /// is the first phase with both the function's concrete return type and
    /// the case/field symbol tables (`enum_case_by_name`/`members`) populated.
    /// Every other intrinsic passes through unchanged.
    fn resolve_intrinsic_payload(
        &self,
        intrinsic: hir::Intrinsic,
        return_type: &hir::Type,
    ) -> hir::Intrinsic {
        match intrinsic {
            hir::Intrinsic::FileReadAllText(_) => {
                hir::Intrinsic::FileReadAllText(self.resolve_file_io_result_layout(return_type))
            }
            hir::Intrinsic::FileWriteAllText(_) => {
                hir::Intrinsic::FileWriteAllText(self.resolve_file_io_result_layout(return_type))
            }
            hir::Intrinsic::FileAppendAllText(_) => {
                hir::Intrinsic::FileAppendAllText(self.resolve_file_io_result_layout(return_type))
            }
            hir::Intrinsic::FileListFiles(_) => {
                hir::Intrinsic::FileListFiles(self.resolve_file_io_result_layout(return_type))
            }
            hir::Intrinsic::FileListDirectories(_) => {
                hir::Intrinsic::FileListDirectories(self.resolve_file_io_result_layout(return_type))
            }
            hir::Intrinsic::FileExists(_) => {
                hir::Intrinsic::FileExists(self.resolve_file_io_result_layout(return_type))
            }
            hir::Intrinsic::DirectoryExists(_) => {
                hir::Intrinsic::DirectoryExists(self.resolve_file_io_result_layout(return_type))
            }
            hir::Intrinsic::FileCreateDirectory(_) => {
                hir::Intrinsic::FileCreateDirectory(self.resolve_file_io_result_layout(return_type))
            }
            hir::Intrinsic::FileDeleteFile(_) => {
                hir::Intrinsic::FileDeleteFile(self.resolve_file_io_result_layout(return_type))
            }
            hir::Intrinsic::FileDeleteDirectory(_) => {
                hir::Intrinsic::FileDeleteDirectory(self.resolve_file_io_result_layout(return_type))
            }
            other => other,
        }
    }

    /// Resolves every symbol `aster.io.ReadAllText`/`WriteAllText` need to
    /// construct a `Result<T, IOError>` value, using the exact same
    /// case/field resolution ordinary `Result<T,E>.Ok(...)`/`.Error(...)`
    /// expressions already go through (`enum_case_by_name`, populated
    /// alongside `enum_cases` in `predeclare`) -- never a name comparison
    /// left for the backend. `return_type` must already be the concrete,
    /// monomorphized `Result<T, IOError>` HIR lowering resolved for this
    /// function's declared signature.
    fn resolve_file_io_result_layout(&self, return_type: &hir::Type) -> hir::FileIoResultLayout {
        let hir::Type::Enum(result_symbol) = return_type else {
            panic!("aster.io.ReadAllText/WriteAllText must return a concrete Result<T, IOError>");
        };
        let result_name = self.type_name_of(*result_symbol);
        let (ok_case, ok_fields) =
            self.enum_case_by_name[&(result_name.to_owned(), "Ok".to_owned())].clone();
        let (error_case, error_fields) =
            self.enum_case_by_name[&(result_name.to_owned(), "Error".to_owned())].clone();
        let [ok_field] = ok_fields[..] else {
            panic!("aster.core.Result<T, IOError>'s Ok case must carry exactly one field");
        };
        let [error_field] = error_fields[..] else {
            panic!("aster.core.Result<T, IOError>'s Error case must carry exactly one field");
        };
        let hir::Type::User(io_error_symbol) = self.symbol_types[&error_field] else {
            panic!("aster.io.IOError must be a concrete struct");
        };
        let io_error_kind_field = self.members[&io_error_symbol]["Kind"];
        let io_error_os_code_field = self.members[&io_error_symbol]["OsCode"];
        let hir::Type::Enum(io_error_kind_symbol) = self.symbol_types[&io_error_kind_field] else {
            panic!("aster.io.IOError.Kind must be a concrete enum");
        };
        let io_error_kind_name = self.type_name_of(io_error_kind_symbol);
        let portable_kind_cases = [
            "NotFound",
            "PermissionDenied",
            "AlreadyExists",
            "InvalidPath",
            "InvalidUtf8",
            "NotFile",
            "NotDirectory",
            "LimitExceeded",
            "Other",
        ]
        .map(|name| self.enum_case_by_name[&(io_error_kind_name.to_owned(), name.to_owned())].0);
        hir::FileIoResultLayout {
            ok_case,
            ok_field,
            error_case,
            error_field,
            io_error_kind_field,
            io_error_os_code_field,
            portable_kind_cases,
        }
    }

    /// Reverse lookup of `self.types` (name -> symbol): the concrete type
    /// symbols `resolve_file_io_result_layout` starts from (`Result<T,
    /// IOError>`'s own symbol, then `IOError`'s, then `IOErrorKind`'s) are
    /// known, but `enum_case_by_name` is indexed by declared name, so their
    /// name must be recovered once to look their cases up.
    fn type_name_of(&self, symbol: hir::SymbolId) -> &str {
        self.types
            .iter()
            .find(|&(_, &value)| value == symbol)
            .map(|(name, _)| name.as_str())
            .expect("every resolved type symbol has a registered name")
    }

    fn field_initializer(
        &mut self,
        owner: &ast::TypeDeclaration,
        field: &ast::Field,
        initializer: &ast::Expression,
    ) -> hir::Expression {
        let previous_context = std::mem::replace(
            &mut self.model_context,
            crate::semantic::field_context(&owner.name, &field.name),
        );
        let initializer = self.expression(initializer);
        self.model_context = previous_context;
        initializer
    }

    /// How `function` is bound. An override carries the symbol of the member
    /// semantic analysis matched it with.
    fn method_dispatch(
        &self,
        function: &ast::FunctionDeclaration,
        owner: Option<&ast::TypeDeclaration>,
        accessor: Option<crate::semantic::AccessorKind>,
    ) -> hir::MethodDispatch {
        match function.dispatch {
            aster_syntax::MemberDispatch::None => hir::MethodDispatch::None,
            aster_syntax::MemberDispatch::Virtual => hir::MethodDispatch::Virtual,
            aster_syntax::MemberDispatch::Abstract => hir::MethodDispatch::Abstract,
            aster_syntax::MemberDispatch::Override => {
                let key = crate::semantic::callable_key(
                    &function.name,
                    function.span.start,
                    if accessor.is_some() {
                        &[]
                    } else {
                        &function.parameters
                    },
                    accessor,
                    owner.map(|owner| owner.name.as_str()),
                );
                let overridden = self
                    .model
                    .overrides
                    .get(&key)
                    .expect("a validated override names the member it overrides");
                hir::MethodDispatch::Override(self.callable_symbols[overridden])
            }
        }
    }

    /// `class` followed by its base classes, nearest first.
    pub(super) fn class_chain(&self, class: hir::SymbolId) -> Vec<hir::SymbolId> {
        let mut chain = vec![class];
        while let Some(base) = self
            .class_bases
            .get(chain.last().expect("chain is not empty"))
        {
            chain.push(*base);
        }
        chain
    }

    /// Whether `base` is a strict ancestor of `class`.
    pub(super) fn derives_from(&self, class: hir::SymbolId, base: hir::SymbolId) -> bool {
        class != base && self.class_chain(class).contains(&base)
    }

    /// Member `name` of `owner` or of its nearest base declaring it.
    pub(super) fn lookup_member(&self, owner: hir::SymbolId, name: &str) -> Option<hir::SymbolId> {
        self.class_chain(owner)
            .into_iter()
            .find_map(|class| self.members.get(&class)?.get(name).copied())
    }

    /// The receiver typed as the class that declares `member`: a member
    /// inherited from a base is reached through an identity upcast.
    pub(super) fn upcast_receiver(
        &self,
        object: hir::Expression,
        member: hir::SymbolId,
    ) -> hir::Expression {
        let (hir::Type::Class(from), Some(owner)) =
            (&object.type_, self.member_owners.get(&member))
        else {
            return object;
        };
        if from == owner || !self.class_types.contains(owner) {
            return object;
        }
        hir::Expression {
            type_: hir::Type::Class(*owner),
            kind: hir::ExpressionKind::UpcastClass {
                object: Box::new(object),
            },
        }
    }

    /// Statements that open a constructor body, in the C# construction order:
    /// the class's field initializers, then the base constructor call
    /// (arguments evaluated left to right, then the base constructor, which
    /// runs the same sequence for its own class), then the declared body.
    fn prepend_constructor_prologue(
        &mut self,
        owner: &ast::TypeDeclaration,
        function: &ast::FunctionDeclaration,
        receiver: hir::SymbolId,
        body: &mut hir::Block,
    ) {
        self.prepend_field_initializers(owner, receiver, body);
        let Some(resolved) = self
            .model
            .base_constructors
            .get(&self.model_context)
            .cloned()
        else {
            return;
        };
        let constructor = self.callable_symbols[&resolved.callable];
        let parameter_types = self.callable_parameters[&constructor].clone();
        let arguments = function
            .base_call
            .as_ref()
            .map_or(&[][..], |call| call.arguments.as_slice());
        let mut lowered = arguments
            .iter()
            .zip(&resolved.arguments.source_to_parameter)
            .map(|(argument, parameter)| {
                convert(
                    self.expression(&argument.value),
                    &parameter_types[*parameter],
                )
            })
            .collect::<Vec<_>>();
        let mut argument_order = resolved.arguments.source_to_parameter.clone();
        for default in &resolved.arguments.defaults {
            lowered.push(convert(
                constant_expression(&default.value),
                &parameter_types[default.parameter],
            ));
            argument_order.push(default.parameter);
        }
        let this = hir::Expression {
            type_: self.symbol_types[&receiver].clone(),
            kind: hir::ExpressionKind::Symbol(receiver),
        };
        let object = self.upcast_receiver(this, constructor);
        let call = hir::Statement::Expression(hir::Expression {
            type_: hir::Type::Void,
            kind: hir::ExpressionKind::Call {
                callee: Box::new(hir::Expression {
                    type_: hir::Type::Unknown,
                    kind: hir::ExpressionKind::Member {
                        object: Box::new(object),
                        symbol: constructor,
                    },
                }),
                arguments: lowered,
                argument_order,
            },
        });
        // `prepend_field_initializers` already placed the initializers first:
        // the base call goes right after them, before the declared body.
        let initializer_count = owner
            .members
            .iter()
            .filter(
                |member| matches!(member, ast::Member::Field(field) if field.initializer.is_some()),
            )
            .count();
        body.statements.insert(initializer_count, call);
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
        base_call: None,
        dispatch: property.dispatch,
        constructor: false,
        is_test: false,
        is_static: false,
        is_async: false,
        is_foreign: false,
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
                default: None,
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
