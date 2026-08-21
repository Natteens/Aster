use super::{BackendError, HashMap, HashSet, integer_constant_bits, mir, primitive, type_name};
use std::collections::BTreeMap;

mod array_bounds;

pub(super) fn select_entry<'a>(
    module: &'a mir::Module,
    function_name: &str,
) -> Result<&'a mir::Function, BackendError> {
    let function = module
        .functions
        .iter()
        .find(|function| function.name == function_name && function.owner.is_none())
        .ok_or_else(|| BackendError::new(format!("function `{function_name}` was not found")))?;
    validate_invocable_entry(function, function_name)?;
    Ok(function)
}

pub(super) fn validate_invocable_entry(
    function: &mir::Function,
    function_name: &str,
) -> Result<(), BackendError> {
    if function.visibility != mir::Visibility::Public {
        return Err(BackendError::new(format!(
            "function `{function_name}` is not public"
        )));
    }
    if !function.parameters.is_empty() {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` must have no parameters"
        )));
    }
    if matches!(function.return_type, mir::Type::User(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns a struct; call it from a scalar entry function instead"
        )));
    }
    if matches!(function.return_type, mir::Type::Array(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns an array; call it from a scalar entry function instead"
        )));
    }
    if matches!(function.return_type, mir::Type::Class(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns an object reference; call it from a scalar entry function instead"
        )));
    }
    if matches!(function.return_type, mir::Type::Interface(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns an interface reference; call it from a scalar entry function instead"
        )));
    }
    if matches!(function.return_type, mir::Type::Enum(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns an enum; handle it in a scalar entry function instead"
        )));
    }
    if matches!(function.return_type, mir::Type::Task(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns a Task<T>; call it from a scalar entry function and `Wait()` there instead"
        )));
    }
    if matches!(function.return_type, mir::Type::List(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns a List<T>; call it from a scalar entry function instead"
        )));
    }
    if matches!(function.return_type, mir::Type::Dictionary(_, _)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns a Dictionary<K, V>; call it from a scalar entry function instead"
        )));
    }
    Ok(())
}

pub(super) fn validate_module(module: &mir::Module) -> Result<(), BackendError> {
    let mut signatures = HashMap::new();
    for function in &module.functions {
        if signatures.insert(function.symbol, function).is_some() {
            return Err(BackendError::new(format!(
                "duplicate function symbol {:?} in MIR",
                function.symbol
            )));
        }
    }
    validate_foreign_abi(module, &signatures)?;
    let classes = module
        .classes
        .iter()
        .map(|class| class.symbol)
        .collect::<HashSet<_>>();
    // `List<T>`'s element can nominally be any of these; existence is
    // checked the same way `AllocateObject` already checks `classes`.
    let structs = module
        .structs
        .iter()
        .map(|definition| definition.symbol)
        .collect::<HashSet<_>>();
    let interfaces = module
        .interfaces
        .iter()
        .map(|interface| interface.symbol)
        .collect::<HashSet<_>>();
    let enums = module
        .enums
        .iter()
        .map(|definition| definition.symbol)
        .collect::<HashSet<_>>();
    let (interface_methods, implementations) =
        validate_interface_metadata(module, &signatures, &classes)?;
    let owned_effects = owned_region_effects(module, &signatures);
    for function in &module.functions {
        validate_function(
            function,
            &signatures,
            &owned_effects,
            &classes,
            &structs,
            &interfaces,
            &enums,
            &interface_methods,
            &implementations,
        )?;
    }
    let enum_definitions = module
        .enums
        .iter()
        .map(|definition| (definition.symbol, definition))
        .collect::<HashMap<_, _>>();
    let struct_definitions = module
        .structs
        .iter()
        .map(|definition| (definition.symbol, definition))
        .collect::<HashMap<_, _>>();
    validate_task_argument_transfer(module, &struct_definitions, &enum_definitions)?;
    for function in &module.functions {
        validate_string_try_parse_targets(function, &enum_definitions)?;
        validate_file_io_result_shapes(function, &enum_definitions, &struct_definitions)?;
        validate_enum_construct_shapes(function, &enum_definitions)?;
        validate_struct_literal_shapes(function, &struct_definitions)?;
    }
    validate_no_console_io_in_workers(module)?;
    Ok(())
}

/// Console/filesystem I/O and foreign calls are rejected anywhere reachable from a
/// `Task.Run`/`Parallel.For`/`ForEach`/`Reduce` worker body: output order,
/// input consumption, and file access would be non-deterministic across
/// workers, and neither backend is shared or synchronized. Builds the whole
/// module's direct call graph once (`Instruction::Call` edges), marks every
/// function whose body directly calls a host I/O intrinsic or foreign binding,
/// then, for every worker entry point found anywhere in the module, walks
/// the graph from its `Function` operand(s) to see whether an I/O-using
/// function is reachable.
fn validate_no_console_io_in_workers(module: &mir::Module) -> Result<(), BackendError> {
    let mut callees: HashMap<mir::SymbolId, Vec<mir::SymbolId>> = HashMap::new();
    let mut io_users: HashSet<mir::SymbolId> = HashSet::new();
    let mut foreign_users: HashSet<mir::SymbolId> = HashSet::new();
    for function in &module.functions {
        let mut direct = Vec::new();
        for block in &function.blocks {
            for instruction in &block.instructions {
                match instruction {
                    mir::Instruction::Call {
                        function: callee, ..
                    } => direct.push(*callee),
                    mir::Instruction::CallIntrinsic {
                        intrinsic:
                            mir::Intrinsic::ConsoleWrite
                            | mir::Intrinsic::ConsoleWriteLine
                            | mir::Intrinsic::ConsoleReadLine
                            | mir::Intrinsic::ConsoleReadLineTemporary
                            | mir::Intrinsic::FileReadAllText(_)
                            | mir::Intrinsic::FileReadAllTextTemporary(_)
                            | mir::Intrinsic::FileWriteAllText(_)
                            | mir::Intrinsic::FileListFiles(_)
                            | mir::Intrinsic::FileListFilesTemporary(_),
                        ..
                    } => {
                        io_users.insert(function.symbol);
                    }
                    mir::Instruction::ForeignCall { .. } => {
                        foreign_users.insert(function.symbol);
                    }
                    _ => {}
                }
            }
        }
        callees.insert(function.symbol, direct);
    }
    let reaches = |root: mir::SymbolId, users: &HashSet<mir::SymbolId>| -> bool {
        let mut visited: HashSet<mir::SymbolId> = HashSet::new();
        let mut stack = vec![root];
        while let Some(symbol) = stack.pop() {
            if !visited.insert(symbol) {
                continue;
            }
            if users.contains(&symbol) {
                return true;
            }
            if let Some(direct) = callees.get(&symbol) {
                stack.extend(direct.iter().copied());
            }
        }
        false
    };
    for function in &module.functions {
        for block in &function.blocks {
            for instruction in &block.instructions {
                let mir::Instruction::CallIntrinsic {
                    intrinsic,
                    arguments,
                    ..
                } = instruction
                else {
                    continue;
                };
                let worker_name = match intrinsic {
                    mir::Intrinsic::TaskRun => "Task.Run",
                    mir::Intrinsic::ParallelFor => "Parallel.For",
                    mir::Intrinsic::ParallelForEach => "Parallel.ForEach",
                    mir::Intrinsic::ParallelReduce => "Parallel.Reduce",
                    _ => continue,
                };
                for argument in arguments {
                    if let mir::OperandKind::Function(target) = argument.kind
                        && reaches(target, &foreign_users)
                    {
                        return Err(BackendError::new(format!(
                            "function `{}` uses `{worker_name}` with a worker body that (directly or transitively) performs a foreign call, which is rejected in this version",
                            function.name
                        )));
                    }
                    if let mir::OperandKind::Function(target) = argument.kind
                        && reaches(target, &io_users)
                    {
                        return Err(BackendError::new(format!(
                            "function `{}` uses `{worker_name}` with a worker body that (directly or transitively) calls `aster.io.Write`/`WriteLine`/`ReadLine`/`ReadAllText`/`WriteAllText`/`ListFiles`, which is rejected in this version",
                            function.name
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn foreign_scalar(type_: &mir::Type, allow_void: bool) -> bool {
    (allow_void && *type_ == mir::Type::Void)
        || matches!(
            type_,
            mir::Type::Bool
                | mir::Type::SByte
                | mir::Type::Byte
                | mir::Type::Short
                | mir::Type::UShort
                | mir::Type::Char
                | mir::Type::Int
                | mir::Type::UInt
                | mir::Type::Long
                | mir::Type::ULong
                | mir::Type::Float
                | mir::Type::Double
        )
}

#[allow(clippy::too_many_lines)]
fn validate_foreign_abi(
    module: &mir::Module,
    functions: &HashMap<mir::SymbolId, &mir::Function>,
) -> Result<(), BackendError> {
    let mut declarations = HashMap::new();
    let mut binding_signatures = Vec::new();
    for declaration in &module.foreign_functions {
        if functions.contains_key(&declaration.symbol)
            || declarations
                .insert(declaration.symbol, declaration)
                .is_some()
        {
            return Err(BackendError::new(format!(
                "duplicate foreign function symbol {:?} in MIR",
                declaration.symbol
            )));
        }
        if declaration.name.is_empty()
            || !foreign_scalar(&declaration.return_type, true)
            || declaration
                .parameters
                .iter()
                .any(|parameter| !foreign_scalar(parameter, false))
        {
            return Err(BackendError::new(format!(
                "foreign declaration `{}` has an invalid scalar ABI signature",
                declaration.name
            )));
        }
        if binding_signatures.iter().any(
            |(name, parameters, result): &(&str, &[mir::Type], &mir::Type)| {
                *name == declaration.name
                    && *parameters == declaration.parameters
                    && *result == &declaration.return_type
            },
        ) {
            return Err(BackendError::new(format!(
                "duplicate foreign binding identity `{}` with the same signature in MIR",
                declaration.name
            )));
        }
        binding_signatures.push((
            declaration.name.as_str(),
            declaration.parameters.as_slice(),
            &declaration.return_type,
        ));
    }
    for function in &module.functions {
        let locals = function
            .parameters
            .iter()
            .chain(&function.locals)
            .map(|local| (local.id, &local.type_))
            .collect::<HashMap<_, _>>();
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            let mir::Instruction::ForeignCall {
                destination,
                function: target,
                arguments,
                return_type,
            } = instruction
            else {
                continue;
            };
            let declaration = declarations.get(target).ok_or_else(|| {
                BackendError::new(format!(
                    "function `{}` calls undeclared foreign symbol {:?}",
                    function.name, target
                ))
            })?;
            if return_type != &declaration.return_type
                || arguments.len() != declaration.parameters.len()
                || arguments
                    .iter()
                    .zip(&declaration.parameters)
                    .any(|(argument, parameter)| &argument.type_ != parameter)
            {
                return Err(BackendError::new(format!(
                    "function `{}` calls foreign declaration `{}` with an invalid signature",
                    function.name, declaration.name
                )));
            }
            for argument in arguments {
                validate_operand(argument, &function.name)?;
            }
            match (return_type, destination) {
                (mir::Type::Void, None) => {}
                (mir::Type::Void, Some(_)) | (_, None) => {
                    return Err(BackendError::new(format!(
                        "function `{}` has an invalid foreign result destination",
                        function.name
                    )));
                }
                (_, Some(mir::Place::Local(local)))
                    if locals.get(local).is_some_and(|type_| *type_ == return_type) => {}
                _ => {
                    return Err(BackendError::new(format!(
                        "function `{}` has a foreign result destination with the wrong type",
                        function.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Every `EnumConstruct` (built by enum literals, `switch` desugaring, and
/// postfix `?`'s `Result`/`Option` propagation alike) must name a `case`
/// symbol that is actually one of `value.type_`'s declared cases, with a
/// matching `tag` and exactly the fields that case declares (by symbol and
/// type, in order). Runs once per function over the whole module's enum
/// definitions, the same data `validate_string_try_parse_targets` uses,
/// rather than a construct-site-specific check duplicated per caller.
fn validate_enum_construct_shapes(
    function: &mir::Function,
    enum_definitions: &HashMap<mir::SymbolId, &mir::EnumDefinition>,
) -> Result<(), BackendError> {
    for block in &function.blocks {
        for instruction in &block.instructions {
            let mir::Instruction::Assign { value, .. } = instruction else {
                continue;
            };
            let mir::RvalueKind::EnumConstruct { case, tag, fields } = &value.kind else {
                continue;
            };
            let mir::Type::Enum(symbol) = &value.type_ else {
                return Err(BackendError::new(format!(
                    "function `{}` has an `EnumConstruct` whose declared type is not an enum",
                    function.name
                )));
            };
            let definition = enum_definitions.get(symbol).ok_or_else(|| {
                BackendError::new(format!(
                    "function `{}` has an `EnumConstruct` for an unknown enum type",
                    function.name
                ))
            })?;
            let matched = definition
                .cases
                .iter()
                .find(|case_definition| case_definition.symbol == *case);
            let Some(matched) = matched else {
                return Err(BackendError::new(format!(
                    "function `{}` has an `EnumConstruct` whose case symbol is not one of `{}`'s cases",
                    function.name, definition.name
                )));
            };
            if matched.tag != *tag {
                return Err(BackendError::new(format!(
                    "function `{}` has an `EnumConstruct` for case `{}` with tag {tag}, expected {}",
                    function.name, matched.name, matched.tag
                )));
            }
            if fields.len() != matched.fields.len() {
                return Err(BackendError::new(format!(
                    "function `{}` has an `EnumConstruct` for case `{}` with {} field(s), expected {}",
                    function.name,
                    matched.name,
                    fields.len(),
                    matched.fields.len()
                )));
            }
            for (provided, expected) in fields.iter().zip(&matched.fields) {
                if provided.field != expected.symbol || provided.value.type_ != expected.type_ {
                    return Err(BackendError::new(format!(
                        "function `{}` has an `EnumConstruct` for case `{}` with a field that does not match its declaration",
                        function.name, matched.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Every `Aggregate` (a struct literal, e.g. `IOError { Kind: ..., OsCode:
/// ... }`) must name a `Type::User` struct that actually exists, with
/// exactly the fields that struct declares, by symbol, order, and type.
/// Mirrors [`validate_enum_construct_shapes`]: previously `Aggregate`'s only
/// check was that each field operand was itself well-formed
/// (`validate_rvalue`), never that the *set* of fields matched the struct's
/// real declaration, so adulterated MIR reached Cranelift's own verifier
/// instead of a controlled `BackendError`.
fn validate_struct_literal_shapes(
    function: &mir::Function,
    struct_definitions: &HashMap<mir::SymbolId, &mir::StructDefinition>,
) -> Result<(), BackendError> {
    for block in &function.blocks {
        for instruction in &block.instructions {
            let mir::Instruction::Assign { value, .. } = instruction else {
                continue;
            };
            let mir::RvalueKind::Aggregate(fields) = &value.kind else {
                continue;
            };
            let mir::Type::User(symbol) = &value.type_ else {
                return Err(BackendError::new(format!(
                    "function `{}` has an `Aggregate` whose declared type is not a struct",
                    function.name
                )));
            };
            let definition = struct_definitions.get(symbol).ok_or_else(|| {
                BackendError::new(format!(
                    "function `{}` has an `Aggregate` for an unknown struct type",
                    function.name
                ))
            })?;
            if fields.len() != definition.fields.len() {
                return Err(BackendError::new(format!(
                    "function `{}` has an `Aggregate` for `{}` with {} field(s), expected {}",
                    function.name,
                    definition.name,
                    fields.len(),
                    definition.fields.len()
                )));
            }
            for (provided, expected) in fields.iter().zip(&definition.fields) {
                if provided.field != expected.symbol || provided.value.type_ != expected.type_ {
                    return Err(BackendError::new(format!(
                        "function `{}` has an `Aggregate` for `{}` with a field that does not match its declaration",
                        function.name, definition.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// The exact `Option<T>` shape `string.TryParse*()`/`aster.io.ReadLine()`
/// must return: precisely two cases, one carrying zero fields (`None`) and
/// the other carrying exactly one field of the type `intrinsic` targets
/// (`Some`). Runs once per function over the whole module's enum
/// definitions -- data `validate_intrinsic_shape` does not have -- rather
/// than threading it through every layer of the generic per-instruction
/// validators above.
fn validate_string_try_parse_targets(
    function: &mir::Function,
    enum_definitions: &HashMap<mir::SymbolId, &mir::EnumDefinition>,
) -> Result<(), BackendError> {
    for block in &function.blocks {
        for instruction in &block.instructions {
            let mir::Instruction::CallIntrinsic {
                intrinsic,
                return_type,
                ..
            } = instruction
            else {
                continue;
            };
            let expected = match intrinsic {
                mir::Intrinsic::StringTryParseBool => mir::Type::Bool,
                mir::Intrinsic::StringTryParseInt => mir::Type::Int,
                mir::Intrinsic::StringTryParseUInt => mir::Type::UInt,
                mir::Intrinsic::StringTryParseLong => mir::Type::Long,
                mir::Intrinsic::StringTryParseULong => mir::Type::ULong,
                mir::Intrinsic::StringTryParseFloat => mir::Type::Float,
                mir::Intrinsic::StringTryParseDouble => mir::Type::Double,
                mir::Intrinsic::ConsoleReadLine | mir::Intrinsic::ConsoleReadLineTemporary => {
                    mir::Type::String
                }
                _ => continue,
            };
            let mir::Type::Enum(symbol) = return_type else {
                return Err(BackendError::new(format!(
                    "function `{}` has {intrinsic:?} returning `{}`, expected `Option<{}>`",
                    function.name,
                    type_name_owned(return_type),
                    type_name(&expected)
                )));
            };
            let definition = enum_definitions.get(symbol).ok_or_else(|| {
                BackendError::new(format!(
                    "function `{}` has {intrinsic:?} returning an unknown enum type",
                    function.name
                ))
            })?;
            let none_case = definition.cases.iter().find(|case| case.fields.is_empty());
            let some_case = definition
                .cases
                .iter()
                .find(|case| case.fields.len() == 1 && case.fields[0].type_ == expected);
            if definition.cases.len() != 2 || none_case.is_none() || some_case.is_none() {
                return Err(BackendError::new(format!(
                    "function `{}` has {intrinsic:?} returning `{}`, which is not `Option<{}>`",
                    function.name,
                    definition.name,
                    type_name(&expected)
                )));
            }
        }
    }
    Ok(())
}

/// The exact `Result<T, IOError>` shape `aster.io.ReadAllText`/
/// `WriteAllText` must return, checked by *symbol* equality against the
/// `hir::FileIoResultLayout` HIR lowering resolved once for this intrinsic
/// (never by comparing a case/field name here): the `Ok`/`Error` cases and
/// `IOError`'s `Kind`/`OsCode` fields the intrinsic carries must actually
/// exist, with matching symbols, in the concrete `Result`/`IOError`
/// definitions the return type and `Error` payload resolve to. Runs once per
/// function over the whole module's enum/struct definitions -- the same data
/// `validate_string_try_parse_targets` uses for `Option<T>` -- so `aster
/// check` rejects adulterated MIR here, before `aster run`'s codegen would
/// (`Codegen::result_io_error_layout`) independently reject the same thing.
fn validate_file_io_result_shapes(
    function: &mir::Function,
    enum_definitions: &HashMap<mir::SymbolId, &mir::EnumDefinition>,
    struct_definitions: &HashMap<mir::SymbolId, &mir::StructDefinition>,
) -> Result<(), BackendError> {
    for block in &function.blocks {
        for instruction in &block.instructions {
            let mir::Instruction::CallIntrinsic {
                intrinsic,
                return_type,
                ..
            } = instruction
            else {
                continue;
            };
            let (expected_ok, layout) = match intrinsic {
                mir::Intrinsic::FileReadAllText(layout)
                | mir::Intrinsic::FileReadAllTextTemporary(layout) => (mir::Type::String, layout),
                mir::Intrinsic::FileWriteAllText(layout) => (mir::Type::Int, layout),
                mir::Intrinsic::FileListFiles(layout)
                | mir::Intrinsic::FileListFilesTemporary(layout) => {
                    (mir::Type::Array(Box::new(mir::Type::String)), layout)
                }
                _ => continue,
            };
            let malformed = || {
                BackendError::new(format!(
                    "function `{}` has {intrinsic:?} returning `{}`, which is not `Result<{}, IOError>`",
                    function.name,
                    type_name_owned(return_type),
                    type_name(&expected_ok)
                ))
            };
            let mir::Type::Enum(symbol) = return_type else {
                return Err(malformed());
            };
            let definition = enum_definitions.get(symbol).ok_or_else(malformed)?;
            let ok_case = definition
                .cases
                .iter()
                .find(|case| case.symbol == layout.ok_case)
                .ok_or_else(malformed)?;
            let error_case = definition
                .cases
                .iter()
                .find(|case| case.symbol == layout.error_case)
                .ok_or_else(malformed)?;
            if !matches!(ok_case.fields.as_slice(), [field] if field.symbol == layout.ok_field && field.type_ == expected_ok)
            {
                return Err(malformed());
            }
            let [error_field] = error_case.fields.as_slice() else {
                return Err(malformed());
            };
            if error_field.symbol != layout.error_field {
                return Err(malformed());
            }
            let mir::Type::User(io_error_symbol) = &error_field.type_ else {
                return Err(malformed());
            };
            let io_error_definition = struct_definitions
                .get(io_error_symbol)
                .ok_or_else(malformed)?;
            let has_kind = io_error_definition.fields.iter().any(|field| {
                field.symbol == layout.io_error_kind_field
                    && matches!(field.type_, mir::Type::Enum(_))
            });
            let has_oscode = io_error_definition.fields.iter().any(|field| {
                field.symbol == layout.io_error_os_code_field && field.type_ == mir::Type::Int
            });
            if !has_kind || !has_oscode {
                return Err(malformed());
            }
        }
    }
    Ok(())
}

type InterfaceMethods<'a> =
    HashMap<mir::SymbolId, (mir::SymbolId, &'a mir::InterfaceMethodDefinition)>;

fn validate_interface_metadata<'a>(
    module: &'a mir::Module,
    signatures: &HashMap<mir::SymbolId, &'a mir::Function>,
    classes: &HashSet<mir::SymbolId>,
) -> Result<
    (
        InterfaceMethods<'a>,
        HashSet<(mir::SymbolId, mir::SymbolId)>,
    ),
    BackendError,
> {
    let mut methods = HashMap::new();
    let interfaces = module
        .interfaces
        .iter()
        .map(|interface| {
            for method in &interface.methods {
                methods.insert(method.symbol, (interface.symbol, method));
            }
            (interface.symbol, interface)
        })
        .collect::<HashMap<_, _>>();
    let mut implementations = HashSet::new();
    for implementation in &module.interface_implementations {
        if !classes.contains(&implementation.class) {
            return Err(BackendError::new(
                "interface implementation references an unknown class",
            ));
        }
        let interface = interfaces.get(&implementation.interface).ok_or_else(|| {
            BackendError::new("interface implementation references an unknown interface")
        })?;
        if !implementations.insert((implementation.class, implementation.interface)) {
            return Err(BackendError::new(
                "duplicate interface implementation in MIR",
            ));
        }
        if implementation.methods.len() != interface.methods.len() {
            return Err(BackendError::new(
                "interface implementation has the wrong method count",
            ));
        }
        for (required, concrete_symbol) in interface.methods.iter().zip(&implementation.methods) {
            let concrete = signatures.get(concrete_symbol).ok_or_else(|| {
                BackendError::new("interface implementation references an unknown method")
            })?;
            if concrete.owner != Some(implementation.class)
                || concrete.visibility != mir::Visibility::Public
                || concrete.return_type != required.return_type
                || concrete.parameters.first().map(|receiver| &receiver.type_)
                    != Some(&mir::Type::Class(implementation.class))
                || concrete
                    .parameters
                    .iter()
                    .skip(1)
                    .map(|parameter| &parameter.type_)
                    .ne(required.parameters.iter())
            {
                return Err(BackendError::new(format!(
                    "concrete method `{}` does not match interface method `{}`",
                    concrete.name, required.name
                )));
            }
        }
    }
    Ok((methods, implementations))
}

#[allow(clippy::too_many_arguments)]
fn validate_function(
    function: &mir::Function,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    owned_effects: &HashMap<mir::SymbolId, bool>,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
    interface_methods: &InterfaceMethods<'_>,
    implementations: &HashSet<(mir::SymbolId, mir::SymbolId)>,
) -> Result<(), BackendError> {
    if !function.temporary_subregion_candidates.is_empty() {
        return Err(unsupported(
            &function.name,
            "AARM temporary subregion candidates",
        ));
    }
    validate_executable_temporary_subregions(function)?;
    validate_executable_owned_regions(function, owned_effects)?;
    if let Some(owner) = function.owner {
        if !classes.contains(&owner) && !structs.contains(&owner) {
            return Err(BackendError::new(format!(
                "function `{}` has an unknown method owner",
                function.name
            )));
        }
    }
    validate_return_type(&function.return_type, &function.name)?;
    for parameter in &function.parameters {
        validate_value_type(&parameter.type_, &function.name)?;
    }
    for local in &function.locals {
        validate_value_type(&local.type_, &function.name)?;
    }
    let blocks = function
        .blocks
        .iter()
        .map(|block| block.id)
        .collect::<HashSet<_>>();
    if blocks.len() != function.blocks.len() {
        return Err(BackendError::new(format!(
            "function `{}` has duplicate basic block identifiers",
            function.name
        )));
    }
    if !blocks.contains(&function.entry) {
        return Err(BackendError::new(format!(
            "function `{}` references unknown entry block {:?}",
            function.name, function.entry
        )));
    }
    let declared_local_count = function.parameters.len() + function.locals.len();
    let locals = function
        .parameters
        .iter()
        .chain(&function.locals)
        .map(|local| (local.id, local.type_.clone()))
        .collect::<HashMap<_, _>>();
    if locals.len() != declared_local_count {
        return Err(BackendError::new(format!(
            "function `{}` has duplicate local identifiers",
            function.name
        )));
    }
    for block in &function.blocks {
        for instruction in &block.instructions {
            validate_instruction(
                instruction,
                &function.name,
                signatures,
                classes,
                structs,
                interfaces,
                enums,
                &locals,
                interface_methods,
                implementations,
            )?;
        }
        validate_terminator(&block.terminator, &function.name, &blocks)?;
    }
    array_bounds::validate(function)?;
    validate_explicit_array_initialization(function)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
fn validate_instruction(
    instruction: &mir::Instruction,
    function_name: &str,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
    interface_methods: &InterfaceMethods<'_>,
    implementations: &HashSet<(mir::SymbolId, mir::SymbolId)>,
) -> Result<(), BackendError> {
    match instruction {
        mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. }
        | mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::ForeignCall { .. } => Ok(()),
        mir::Instruction::Assign { target, value } => {
            validate_assign(target, value, function_name, locals, implementations)
        }
        mir::Instruction::Call {
            destination,
            function,
            arguments,
            return_type,
        } => validate_call(
            destination.as_ref(),
            *function,
            arguments,
            return_type,
            function_name,
            signatures,
            classes,
            structs,
        ),
        mir::Instruction::CallInterface {
            destination,
            receiver,
            arguments,
            return_type,
            method,
        } => validate_interface_call(
            destination.as_ref(),
            receiver,
            arguments,
            return_type,
            *method,
            function_name,
            interface_methods,
        ),
        mir::Instruction::CallIntrinsic {
            destination,
            intrinsic,
            arguments,
            return_type,
        } => validate_call_intrinsic(
            destination.as_ref(),
            *intrinsic,
            arguments,
            return_type,
            function_name,
            signatures,
            locals,
        ),
        mir::Instruction::AllocateArray {
            destination,
            element_type,
            length,
            initialization,
            ..
        } => validate_allocate_array(
            destination,
            element_type,
            length,
            *initialization,
            function_name,
            locals,
        ),
        mir::Instruction::AllocateObject {
            destination, class, ..
        } => validate_allocate_object(destination, *class, function_name, classes),
        mir::Instruction::AllocateList {
            destination,
            element_type,
            ..
        } => validate_allocate_list(
            destination,
            element_type,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
            locals,
        ),
        mir::Instruction::AllocateDictionary {
            destination,
            key_type,
            value_type,
            ..
        } => validate_allocate_dictionary(
            destination,
            key_type,
            value_type,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
            locals,
        ),
        mir::Instruction::AllocateStringBuilder {
            destination, class, ..
        } => validate_allocate_string_builder(destination, *class, function_name, classes, locals),
        mir::Instruction::StringBuilderAppend {
            builder,
            value,
            class,
        } => validate_string_builder_append(builder, value, *class, function_name, classes, locals),
        mir::Instruction::StringBuilderToString {
            destination,
            builder,
            class,
            ..
        } => validate_string_builder_to_string(
            destination,
            builder,
            *class,
            function_name,
            classes,
            locals,
        ),
        mir::Instruction::DictionaryAdd {
            destination,
            dictionary,
            key,
            value,
        }
        | mir::Instruction::DictionarySet {
            destination,
            dictionary,
            key,
            value,
        } => {
            validate_dictionary_mutation(destination, dictionary, key, value, function_name, locals)
        }
        mir::Instruction::DictionaryTryGet {
            destination,
            dictionary,
            key,
            value_type,
            option_layout,
        } => validate_dictionary_try_get(
            destination,
            dictionary,
            key,
            value_type,
            *option_layout,
            function_name,
            enums,
            locals,
        ),
        mir::Instruction::DictionaryContainsKey {
            destination,
            dictionary,
            key,
        }
        | mir::Instruction::DictionaryRemove {
            destination,
            dictionary,
            key,
        } => validate_dictionary_key_operation(destination, dictionary, key, function_name, locals),
        mir::Instruction::DictionaryEntries {
            destination,
            dictionary,
            key_type,
            value_type,
            entry_type,
            entry_layout,
            ..
        } => validate_dictionary_entries(
            destination,
            dictionary,
            key_type,
            value_type,
            entry_type,
            *entry_layout,
            function_name,
            structs,
            locals,
        ),
        mir::Instruction::DictionaryClear { dictionary } => {
            validate_dictionary_clear(dictionary, function_name, locals)
        }
        mir::Instruction::DictionaryKeys {
            destination,
            dictionary,
            key_type,
            ..
        } => validate_dictionary_snapshot(
            destination,
            dictionary,
            key_type,
            true,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
            locals,
        ),
        mir::Instruction::DictionaryValues {
            destination,
            dictionary,
            value_type,
            ..
        } => validate_dictionary_snapshot(
            destination,
            dictionary,
            value_type,
            false,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
            locals,
        ),
        mir::Instruction::ListAdd { list, value } => validate_list_add(
            list,
            value,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
        ),
        mir::Instruction::ListGet {
            destination,
            list,
            index,
            element_type,
        } => validate_list_get(
            destination,
            list,
            index,
            element_type,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
            locals,
        ),
        mir::Instruction::ListRemoveAt { list, index } => validate_list_remove_at(
            list,
            index,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
        ),
        mir::Instruction::ListSet { list, index, value } => validate_list_set(
            list,
            index,
            value,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
            locals,
        ),
        mir::Instruction::ListClear { list } => validate_list_clear(
            list,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
            locals,
        ),
        mir::Instruction::ListToArray {
            destination,
            list,
            element_type,
            ..
        } => validate_list_to_array(
            destination,
            list,
            element_type,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
            locals,
        ),
        mir::Instruction::StringDecodeNext {
            string,
            cursor,
            char_destination,
            next_cursor_destination,
            ok_destination,
        } => validate_string_decode_next(
            string,
            cursor,
            char_destination,
            next_cursor_destination,
            ok_destination,
            function_name,
            locals,
        ),
    }
}

#[allow(clippy::too_many_lines)]
fn validate_executable_owned_regions(
    function: &mir::Function,
    effects: &HashMap<mir::SymbolId, bool>,
) -> Result<(), BackendError> {
    let locals = function
        .parameters
        .iter()
        .chain(&function.locals)
        .map(|local| (local.id, &local.type_))
        .collect::<HashMap<_, _>>();
    let mut entered = HashSet::new();

    for block in &function.blocks {
        let mut active: Option<(mir::OwnedRegionId, HashSet<mir::LocalId>)> = None;
        for instruction in &block.instructions {
            match instruction {
                mir::Instruction::OwnedRegionEnter { id } => {
                    if active.is_some() || !entered.insert(*id) {
                        return Err(unsupported(
                            &function.name,
                            "nested or duplicate executable owned-region enter",
                        ));
                    }
                    active = Some((*id, HashSet::new()));
                }
                mir::Instruction::OwnedRegionExit { id, invalidated } => {
                    let Some((active_id, produced)) = active.take() else {
                        return Err(unsupported(
                            &function.name,
                            "owned-region exit without a matching enter",
                        ));
                    };
                    let invalidated_set = invalidated.iter().copied().collect::<HashSet<_>>();
                    if active_id != *id
                        || invalidated.is_empty()
                        || invalidated_set.len() != invalidated.len()
                        || invalidated_set.iter().any(|local| {
                            locals
                                .get(local)
                                .is_none_or(|type_| !owned_region_result_type(type_))
                        })
                        || produced.is_empty()
                        || !produced.is_subset(&invalidated_set)
                    {
                        return Err(unsupported(
                            &function.name,
                            "mismatched executable owned-region exit",
                        ));
                    }
                }
                mir::Instruction::TemporarySubregionEnter { .. }
                | mir::Instruction::TemporarySubregionExit { .. }
                    if active.is_some() =>
                {
                    return Err(unsupported(
                        &function.name,
                        "Temporary reclaim inside an executable owned region",
                    ));
                }
                instruction if active.is_some() => {
                    let (_, aliases) = active.as_mut().expect("owned region is active");
                    if aliases.is_empty() {
                        let mir::Instruction::Call {
                            destination: Some(mir::Place::Local(destination)),
                            return_type,
                            ..
                        } = instruction
                        else {
                            return Err(unsupported(
                                &function.name,
                                "owned-region checkpoint not followed by its producer call",
                            ));
                        };
                        if !owned_region_result_type(return_type) {
                            return Err(unsupported(
                                &function.name,
                                "owned-region producer with a non-reference result",
                            ));
                        }
                        aliases.insert(*destination);
                        continue;
                    }
                    if owned_region_barrier(instruction, effects) {
                        return Err(unsupported(
                            &function.name,
                            "unrelated Persistent effect inside an executable owned region",
                        ));
                    }
                    if let mir::Instruction::Assign {
                        target: mir::Place::Local(destination),
                        value,
                    } = instruction
                        && owned_alias_source(value, aliases)
                    {
                        aliases.insert(*destination);
                    }
                }
                _ => {}
            }
        }
        if active.is_some() {
            return Err(unsupported(
                &function.name,
                "executable owned region crosses a basic-block boundary",
            ));
        }
    }

    if entered.is_empty() {
        return Ok(());
    }
    validate_owned_region_invalidated_uses(function)
}

fn owned_region_effects(
    module: &mir::Module,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
) -> HashMap<mir::SymbolId, bool> {
    // This is a fail-closed check over the explicit typed MIR contract, not a
    // second ownership selector: source aliases and last-use stay compiler-owned.
    let mut effects = module
        .functions
        .iter()
        .map(|function| {
            let direct = function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| match instruction {
                    mir::Instruction::Call { function, .. } => !signatures.contains_key(function),
                    _ => owned_region_direct_barrier(instruction),
                });
            (function.symbol, direct)
        })
        .collect::<HashMap<_, _>>();

    loop {
        let mut changed = false;
        for function in &module.functions {
            if effects[&function.symbol] {
                continue;
            }
            let transitive = function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .any(|instruction| {
                    matches!(instruction,
                        mir::Instruction::Call { function, .. }
                            if effects.get(function).copied().unwrap_or(true))
                });
            if transitive {
                effects.insert(function.symbol, true);
                changed = true;
            }
        }
        if !changed {
            return effects;
        }
    }
}

fn owned_region_barrier(
    instruction: &mir::Instruction,
    effects: &HashMap<mir::SymbolId, bool>,
) -> bool {
    match instruction {
        mir::Instruction::Call { function, .. } => effects.get(function).copied().unwrap_or(true),
        _ => owned_region_direct_barrier(instruction),
    }
}

fn owned_region_direct_barrier(instruction: &mir::Instruction) -> bool {
    let persistent_allocation = match instruction {
        mir::Instruction::AllocateObject { region, .. }
        | mir::Instruction::AllocateArray { region, .. }
        | mir::Instruction::AllocateList { region, .. }
        | mir::Instruction::AllocateDictionary { region, .. }
        | mir::Instruction::AllocateStringBuilder { region, .. }
        | mir::Instruction::StringBuilderToString { region, .. }
        | mir::Instruction::DictionaryEntries { region, .. }
        | mir::Instruction::DictionaryKeys { region, .. }
        | mir::Instruction::DictionaryValues { region, .. }
        | mir::Instruction::ListToArray { region, .. } => {
            *region == mir::AllocationRegion::Persistent
        }
        mir::Instruction::CallIntrinsic { intrinsic, .. } => {
            intrinsic.allocation_region() == Some(mir::AllocationRegion::Persistent)
                || is_concurrency_intrinsic(*intrinsic)
        }
        _ => false,
    };
    persistent_allocation
        || matches!(
            instruction,
            mir::Instruction::CallInterface { .. }
                | mir::Instruction::ListAdd { .. }
                | mir::Instruction::DictionaryAdd { .. }
                | mir::Instruction::DictionarySet { .. }
                | mir::Instruction::StringBuilderAppend { .. }
        )
}

fn owned_alias_source(value: &mir::Rvalue, aliases: &HashSet<mir::LocalId>) -> bool {
    matches!(
        &value.kind,
        mir::RvalueKind::Use(mir::Operand {
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
            ..
        }) | mir::RvalueKind::Cast(mir::Operand {
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
            ..
        }) if owned_region_result_type(&value.type_) && aliases.contains(local)
    )
}

fn owned_region_result_type(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::String
            | mir::Type::Array(_)
            | mir::Type::Class(_)
            | mir::Type::List(_)
            | mir::Type::Dictionary(_, _)
    )
}

fn validate_owned_region_invalidated_uses(function: &mir::Function) -> Result<(), BackendError> {
    let blocks = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect::<HashMap<_, _>>();
    let mut incoming = HashMap::from([(function.entry, HashSet::<mir::LocalId>::new())]);
    let mut pending = vec![function.entry];
    while let Some(block_id) = pending.pop() {
        let Some(block) = blocks.get(&block_id) else {
            return Err(unsupported(&function.name, "malformed owned-region CFG"));
        };
        let mut invalid = incoming[&block_id].clone();
        for instruction in &block.instructions {
            let mut reads = HashSet::new();
            owned_instruction_reads(instruction, &mut reads);
            if reads.iter().any(|local| invalid.contains(local)) {
                return Err(unsupported(
                    &function.name,
                    "use of a reclaimed owned-region local before redefinition",
                ));
            }
            let mut definitions = HashSet::new();
            owned_instruction_definitions(instruction, &mut definitions);
            invalid.retain(|local| !definitions.contains(local));
            if let mir::Instruction::OwnedRegionExit { invalidated, .. } = instruction {
                invalid.extend(invalidated.iter().copied());
            }
        }
        let mut terminator_reads = HashSet::new();
        match &block.terminator {
            mir::Terminator::Branch { condition, .. }
            | mir::Terminator::Return(Some(condition)) => {
                owned_operand_reads(condition, &mut terminator_reads);
            }
            _ => {}
        }
        if terminator_reads.iter().any(|local| invalid.contains(local)) {
            return Err(unsupported(
                &function.name,
                "use of a reclaimed owned-region local in a terminator",
            ));
        }
        let successors = match block.terminator {
            mir::Terminator::Goto(target) => vec![target],
            mir::Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![then_block, else_block],
            mir::Terminator::Return(_) | mir::Terminator::End | mir::Terminator::Unreachable => {
                Vec::new()
            }
        };
        for successor in successors {
            if !blocks.contains_key(&successor) {
                return Err(unsupported(&function.name, "malformed owned-region CFG"));
            }
            if let Some(state) = incoming.get_mut(&successor) {
                let old_len = state.len();
                state.extend(invalid.iter().copied());
                if state.len() != old_len {
                    pending.push(successor);
                }
            } else {
                incoming.insert(successor, invalid.clone());
                pending.push(successor);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn owned_instruction_reads(instruction: &mir::Instruction, reads: &mut HashSet<mir::LocalId>) {
    match instruction {
        mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. } => {}
        mir::Instruction::Assign { target, value } => {
            if !matches!(target, mir::Place::Local(_)) {
                owned_place_reads(target, reads);
            }
            owned_rvalue_reads(value, reads);
        }
        mir::Instruction::Call {
            destination,
            arguments,
            ..
        }
        | mir::Instruction::ForeignCall {
            destination,
            arguments,
            ..
        }
        | mir::Instruction::CallIntrinsic {
            destination,
            arguments,
            ..
        } => {
            if let Some(destination) = destination
                && !matches!(destination, mir::Place::Local(_))
            {
                owned_place_reads(destination, reads);
            }
            for argument in arguments {
                owned_operand_reads(argument, reads);
            }
        }
        mir::Instruction::CallInterface {
            destination,
            receiver,
            arguments,
            ..
        } => {
            if let Some(destination) = destination
                && !matches!(destination, mir::Place::Local(_))
            {
                owned_place_reads(destination, reads);
            }
            owned_operand_reads(receiver, reads);
            for argument in arguments {
                owned_operand_reads(argument, reads);
            }
        }
        mir::Instruction::AllocateArray {
            destination,
            length,
            ..
        } => {
            if !matches!(destination, mir::Place::Local(_)) {
                owned_place_reads(destination, reads);
            }
            owned_operand_reads(length, reads);
        }
        mir::Instruction::AllocateObject { destination, .. }
        | mir::Instruction::AllocateList { destination, .. }
        | mir::Instruction::AllocateDictionary { destination, .. }
        | mir::Instruction::AllocateStringBuilder { destination, .. } => {
            owned_destination_reads(destination, reads);
        }
        mir::Instruction::StringBuilderAppend { builder, value, .. } => {
            owned_operand_reads(builder, reads);
            owned_operand_reads(value, reads);
        }
        mir::Instruction::StringBuilderToString {
            destination,
            builder,
            ..
        } => {
            if !matches!(destination, mir::Place::Local(_)) {
                owned_place_reads(destination, reads);
            }
            owned_operand_reads(builder, reads);
        }
        mir::Instruction::DictionaryAdd {
            destination,
            dictionary,
            key,
            value,
        }
        | mir::Instruction::DictionarySet {
            destination,
            dictionary,
            key,
            value,
        } => {
            owned_destination_reads(destination, reads);
            owned_operand_reads(dictionary, reads);
            owned_operand_reads(key, reads);
            owned_operand_reads(value, reads);
        }
        mir::Instruction::DictionaryTryGet {
            destination,
            dictionary,
            key,
            ..
        }
        | mir::Instruction::DictionaryContainsKey {
            destination,
            dictionary,
            key,
        }
        | mir::Instruction::DictionaryRemove {
            destination,
            dictionary,
            key,
        } => {
            owned_destination_reads(destination, reads);
            owned_operand_reads(dictionary, reads);
            owned_operand_reads(key, reads);
        }
        mir::Instruction::DictionaryEntries {
            destination,
            dictionary,
            ..
        }
        | mir::Instruction::DictionaryKeys {
            destination,
            dictionary,
            ..
        }
        | mir::Instruction::DictionaryValues {
            destination,
            dictionary,
            ..
        } => {
            owned_destination_reads(destination, reads);
            owned_operand_reads(dictionary, reads);
        }
        mir::Instruction::DictionaryClear { dictionary } => {
            owned_operand_reads(dictionary, reads);
        }
        mir::Instruction::ListAdd { list, value } => {
            owned_operand_reads(list, reads);
            owned_operand_reads(value, reads);
        }
        mir::Instruction::ListGet {
            destination,
            list,
            index,
            ..
        } => {
            owned_destination_reads(destination, reads);
            owned_operand_reads(list, reads);
            owned_operand_reads(index, reads);
        }
        mir::Instruction::ListRemoveAt { list, index } => {
            owned_operand_reads(list, reads);
            owned_operand_reads(index, reads);
        }
        mir::Instruction::ListSet { list, index, value } => {
            owned_operand_reads(list, reads);
            owned_operand_reads(index, reads);
            owned_operand_reads(value, reads);
        }
        mir::Instruction::ListClear { list } => owned_operand_reads(list, reads),
        mir::Instruction::ListToArray {
            destination, list, ..
        } => {
            owned_destination_reads(destination, reads);
            owned_operand_reads(list, reads);
        }
        mir::Instruction::StringDecodeNext {
            string,
            cursor,
            char_destination,
            next_cursor_destination,
            ok_destination,
        } => {
            owned_operand_reads(string, reads);
            owned_operand_reads(cursor, reads);
            owned_destination_reads(char_destination, reads);
            owned_destination_reads(next_cursor_destination, reads);
            owned_destination_reads(ok_destination, reads);
        }
    }
}

fn owned_instruction_definitions(
    instruction: &mir::Instruction,
    definitions: &mut HashSet<mir::LocalId>,
) {
    match instruction {
        mir::Instruction::Assign { target, .. }
        | mir::Instruction::AllocateArray {
            destination: target,
            ..
        }
        | mir::Instruction::AllocateObject {
            destination: target,
            ..
        }
        | mir::Instruction::AllocateList {
            destination: target,
            ..
        }
        | mir::Instruction::AllocateDictionary {
            destination: target,
            ..
        }
        | mir::Instruction::AllocateStringBuilder {
            destination: target,
            ..
        }
        | mir::Instruction::StringBuilderToString {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryAdd {
            destination: target,
            ..
        }
        | mir::Instruction::DictionarySet {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryTryGet {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryContainsKey {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryRemove {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryEntries {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryKeys {
            destination: target,
            ..
        }
        | mir::Instruction::DictionaryValues {
            destination: target,
            ..
        }
        | mir::Instruction::ListGet {
            destination: target,
            ..
        }
        | mir::Instruction::ListToArray {
            destination: target,
            ..
        } => owned_place_definition(target, definitions),
        mir::Instruction::Call { destination, .. }
        | mir::Instruction::CallInterface { destination, .. }
        | mir::Instruction::CallIntrinsic { destination, .. } => {
            if let Some(destination) = destination {
                owned_place_definition(destination, definitions);
            }
        }
        mir::Instruction::StringDecodeNext {
            char_destination,
            next_cursor_destination,
            ok_destination,
            ..
        } => {
            owned_place_definition(char_destination, definitions);
            owned_place_definition(next_cursor_destination, definitions);
            owned_place_definition(ok_destination, definitions);
        }
        _ => {}
    }
}

fn owned_place_definition(place: &mir::Place, definitions: &mut HashSet<mir::LocalId>) {
    if let mir::Place::Local(local) = place {
        definitions.insert(*local);
    }
}

fn owned_destination_reads(place: &mir::Place, reads: &mut HashSet<mir::LocalId>) {
    if !matches!(place, mir::Place::Local(_)) {
        owned_place_reads(place, reads);
    }
}

fn owned_rvalue_reads(value: &mir::Rvalue, reads: &mut HashSet<mir::LocalId>) {
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::ArrayLength(operand)
        | mir::RvalueKind::ListLength(operand)
        | mir::RvalueKind::DictionaryLength(operand)
        | mir::RvalueKind::ListVersion(operand)
        | mir::RvalueKind::StringByteLength(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => owned_operand_reads(operand, reads),
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            for field in fields {
                owned_operand_reads(&field.value, reads);
            }
        }
        mir::RvalueKind::MakeInterface { object, .. } => owned_operand_reads(object, reads),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            owned_operand_reads(left, reads);
            owned_operand_reads(right, reads);
        }
    }
}

fn owned_operand_reads(operand: &mir::Operand, reads: &mut HashSet<mir::LocalId>) {
    if let mir::OperandKind::Copy(place) = &operand.kind {
        owned_place_reads(place, reads);
    }
}

fn owned_place_reads(place: &mir::Place, reads: &mut HashSet<mir::LocalId>) {
    match place {
        mir::Place::Local(local) => {
            reads.insert(*local);
        }
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            owned_place_reads(base, reads);
        }
        mir::Place::Index { array, index, .. } => {
            owned_operand_reads(array, reads);
            owned_operand_reads(index, reads);
        }
        mir::Place::ObjectField { object, .. } => owned_operand_reads(object, reads),
        mir::Place::Symbol(_) => {}
    }
}

#[allow(clippy::too_many_lines)]
fn validate_executable_temporary_subregions(function: &mir::Function) -> Result<(), BackendError> {
    let contains_subregion = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::TemporarySubregionEnter { .. }
                    | mir::Instruction::TemporarySubregionExit { .. }
            )
        });
    if !contains_subregion {
        return Ok(());
    }

    if function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic { intrinsic, .. }
                    if is_concurrency_intrinsic(*intrinsic)
            )
        })
    {
        return Err(unsupported(
            &function.name,
            "concurrency in an executable AARM temporary subregion function",
        ));
    }

    validate_executable_temporary_subregion_cfg(function)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum FineState {
    Inactive,
    Active(mir::TemporarySubregionId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FineExecutionState {
    fine: FineState,
    owned: BTreeMap<u32, FineOwnedKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FineOwnedKind {
    StringBuilder,
    List,
    Dictionary,
}

impl FineExecutionState {
    fn inactive() -> Self {
        Self {
            fine: FineState::Inactive,
            owned: BTreeMap::new(),
        }
    }

    fn enter(&mut self, id: mir::TemporarySubregionId) -> Result<(), &'static str> {
        if self.fine != FineState::Inactive {
            return Err("nested executable AARM temporary subregion enter");
        }
        self.fine = FineState::Active(id);
        self.owned.clear();
        Ok(())
    }

    fn exit(&mut self, id: mir::TemporarySubregionId) -> Result<(), &'static str> {
        if self.fine != FineState::Active(id) {
            return Err("unmatched executable AARM temporary subregion exit");
        }
        *self = Self::inactive();
        Ok(())
    }

    fn validate_builder_instruction(
        &mut self,
        instruction: &mir::Instruction,
    ) -> Result<(), &'static str> {
        if self.fine == FineState::Inactive {
            return Ok(());
        }
        match instruction {
            mir::Instruction::AllocateStringBuilder {
                destination: mir::Place::Local(local),
                region: mir::AllocationRegion::Temporary,
                ..
            } => {
                if self
                    .owned
                    .insert(local.0, FineOwnedKind::StringBuilder)
                    .is_some()
                {
                    return Err("duplicate executable AARM StringBuilder local");
                }
            }
            mir::Instruction::StringBuilderAppend { builder, .. }
            | mir::Instruction::StringBuilderToString { builder, .. } => {
                let mir::OperandKind::Copy(mir::Place::Local(local)) = builder.kind else {
                    return Err("non-local executable AARM StringBuilder receiver");
                };
                if self.owned.get(&local.0) != Some(&FineOwnedKind::StringBuilder) {
                    return Err("unowned executable AARM StringBuilder receiver");
                }
            }
            mir::Instruction::AllocateList {
                destination: mir::Place::Local(local),
                region: mir::AllocationRegion::Temporary,
                ..
            } => {
                if self.owned.insert(local.0, FineOwnedKind::List).is_some() {
                    return Err("duplicate executable AARM List local");
                }
            }
            mir::Instruction::AllocateDictionary {
                destination: mir::Place::Local(local),
                region: mir::AllocationRegion::Temporary,
                ..
            } => {
                if self
                    .owned
                    .insert(local.0, FineOwnedKind::Dictionary)
                    .is_some()
                {
                    return Err("duplicate executable AARM Dictionary local");
                }
            }
            mir::Instruction::ListAdd { list, .. }
            | mir::Instruction::ListGet { list, .. }
            | mir::Instruction::ListRemoveAt { list, .. } => {
                if !fine_receiver_is_owned(list, FineOwnedKind::List, &self.owned) {
                    return Err("unowned executable AARM List receiver");
                }
            }
            mir::Instruction::DictionaryAdd { dictionary, .. }
            | mir::Instruction::DictionarySet { dictionary, .. }
            | mir::Instruction::DictionaryTryGet { dictionary, .. }
            | mir::Instruction::DictionaryContainsKey { dictionary, .. }
            | mir::Instruction::DictionaryRemove { dictionary, .. } => {
                if !fine_receiver_is_owned(dictionary, FineOwnedKind::Dictionary, &self.owned) {
                    return Err("unowned executable AARM Dictionary receiver");
                }
            }
            mir::Instruction::Assign { target, value }
                if fine_rvalue_mentions_owned_builder(value, &self.owned)
                    || matches!(target, mir::Place::Local(local) if self.owned.contains_key(&local.0)) =>
            {
                return Err("aliased or overwritten executable AARM StringBuilder local");
            }
            _ => {}
        }
        Ok(())
    }
}

fn fine_receiver_is_owned(
    operand: &mir::Operand,
    expected: FineOwnedKind,
    owned: &BTreeMap<u32, FineOwnedKind>,
) -> bool {
    matches!(operand.kind, mir::OperandKind::Copy(mir::Place::Local(local))
        if owned.get(&local.0) == Some(&expected))
}

fn fine_rvalue_mentions_owned_builder(
    value: &mir::Rvalue,
    owned: &BTreeMap<u32, FineOwnedKind>,
) -> bool {
    matches!(&value.kind, mir::RvalueKind::Use(operand) | mir::RvalueKind::Cast(operand)
        if matches!(operand.kind, mir::OperandKind::Copy(mir::Place::Local(local)) if owned.contains_key(&local.0)))
}

#[allow(clippy::too_many_lines)]
fn validate_executable_temporary_subregion_cfg(
    function: &mir::Function,
) -> Result<(), BackendError> {
    let mut blocks = HashMap::new();
    for (index, block) in function.blocks.iter().enumerate() {
        if blocks.insert(block.id, index).is_some() {
            return Err(unsupported(
                &function.name,
                "duplicate basic blocks in executable AARM temporary subregions",
            ));
        }
    }
    if !blocks.contains_key(&function.entry) {
        return Err(unsupported(
            &function.name,
            "missing executable AARM temporary subregion entry",
        ));
    }
    let successors = function
        .blocks
        .iter()
        .map(|block| {
            let successors = match block.terminator {
                mir::Terminator::Goto(target) => vec![target],
                mir::Terminator::Branch {
                    then_block,
                    else_block,
                    ..
                } => vec![then_block, else_block],
                mir::Terminator::Return(_) | mir::Terminator::End => Vec::new(),
                mir::Terminator::Unreachable => return None,
            };
            successors
                .iter()
                .all(|target| blocks.contains_key(target))
                .then_some((block.id, successors))
        })
        .collect::<Option<HashMap<_, _>>>()
        .ok_or_else(|| {
            unsupported(
                &function.name,
                "malformed executable AARM temporary subregion CFG",
            )
        })?;
    let mut reachable = HashSet::new();
    let mut pending = vec![function.entry];
    while let Some(block) = pending.pop() {
        if !reachable.insert(block) {
            continue;
        }
        pending.extend(successors[&block].iter().copied());
    }
    if reachable.len() != blocks.len() {
        return Err(unsupported(
            &function.name,
            "unreachable executable AARM temporary subregion block",
        ));
    }
    validate_executable_temporary_subregion_cycles(function, &successors, &reachable)?;
    let mut state = HashMap::from([(function.entry, FineExecutionState::inactive())]);
    let mut entered = HashMap::new();
    let mut allocations = HashSet::new();
    let mut pending = vec![function.entry];
    while let Some(block_id) = pending.pop() {
        let mut current = state[&block_id].clone();
        let block = &function.blocks[blocks[&block_id]];
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            match instruction {
                mir::Instruction::TemporarySubregionEnter { id } => {
                    if entered
                        .insert(*id, (block_id, instruction_index))
                        .is_some_and(|previous| previous != (block_id, instruction_index))
                        || current.enter(*id).is_err()
                    {
                        return Err(unsupported(
                            &function.name,
                            "nested or duplicate executable AARM temporary subregion enter",
                        ));
                    }
                }
                mir::Instruction::TemporarySubregionExit { id } => {
                    if current.exit(*id).is_err() {
                        return Err(unsupported(
                            &function.name,
                            "unmatched executable AARM temporary subregion exit",
                        ));
                    }
                }
                instruction if current.fine != FineState::Inactive => {
                    current
                        .validate_builder_instruction(instruction)
                        .map_err(|feature| unsupported(&function.name, feature))?;
                    if temporary_subregion_allocation_is_executable(instruction) {
                        allocations.insert(current.fine);
                    }
                    if !temporary_subregion_instruction_is_executable(instruction) {
                        return Err(unsupported(
                            &function.name,
                            "instruction inside an executable AARM temporary subregion",
                        ));
                    }
                }
                _ => {}
            }
        }
        if matches!(
            block.terminator,
            mir::Terminator::Return(_) | mir::Terminator::End
        ) && current.fine != FineState::Inactive
        {
            return Err(unsupported(
                &function.name,
                "Return or End with an active executable AARM temporary subregion",
            ));
        }
        for successor in &successors[&block_id] {
            if let Some(existing) = state.insert(*successor, current.clone()) {
                if existing != current {
                    return Err(unsupported(
                        &function.name,
                        "inconsistent executable AARM temporary subregion state at CFG join",
                    ));
                }
            } else {
                pending.push(*successor);
            }
        }
    }
    if entered
        .keys()
        .any(|id| !allocations.contains(&FineState::Active(*id)))
    {
        return Err(unsupported(
            &function.name,
            "executable AARM temporary subregion without a Temporary allocation",
        ));
    }
    Ok(())
}

fn temporary_subregion_allocation_is_executable(instruction: &mir::Instruction) -> bool {
    matches!(
        instruction,
        mir::Instruction::AllocateObject {
            region: mir::AllocationRegion::Temporary,
            ..
        } | mir::Instruction::AllocateArray {
            region: mir::AllocationRegion::Temporary,
            ..
        } | mir::Instruction::AllocateStringBuilder {
            region: mir::AllocationRegion::Temporary,
            ..
        } | mir::Instruction::StringBuilderToString {
            region: mir::AllocationRegion::Temporary,
            ..
        } | mir::Instruction::AllocateList {
            region: mir::AllocationRegion::Temporary,
            ..
        } | mir::Instruction::AllocateDictionary {
            region: mir::AllocationRegion::Temporary,
            ..
        }
    ) || matches!(
        instruction,
        mir::Instruction::CallIntrinsic {
            intrinsic: mir::Intrinsic::StringConcatTemporary
                | mir::Intrinsic::StringJoinTemporary
                | mir::Intrinsic::StringSubstringFromTemporary
                | mir::Intrinsic::StringSubstringRangeTemporary
                | mir::Intrinsic::StringFromLongTemporary
                | mir::Intrinsic::StringFromULongTemporary
                | mir::Intrinsic::StringFromDoubleTemporary
                | mir::Intrinsic::StringFromFloatTemporary
                | mir::Intrinsic::StringFromBoolTemporary
                | mir::Intrinsic::StringFromCharTemporary,
            ..
        }
    )
}

#[allow(clippy::too_many_lines)]
fn validate_executable_temporary_subregion_cycles(
    function: &mir::Function,
    successors: &HashMap<mir::BasicBlockId, Vec<mir::BasicBlockId>>,
    reachable: &HashSet<mir::BasicBlockId>,
) -> Result<(), BackendError> {
    let mut predecessors = reachable
        .iter()
        .copied()
        .map(|block| (block, Vec::new()))
        .collect::<HashMap<_, _>>();
    for (block, targets) in successors {
        for target in targets {
            predecessors
                .get_mut(target)
                .expect("validated target")
                .push(*block);
        }
    }
    let mut order = reachable.iter().copied().collect::<Vec<_>>();
    order.sort_unstable_by_key(|block| block.0);
    let mut dominators = order
        .iter()
        .map(|block| {
            (
                *block,
                if *block == function.entry {
                    HashSet::from([*block])
                } else {
                    reachable.clone()
                },
            )
        })
        .collect::<HashMap<_, _>>();
    loop {
        let mut changed = false;
        for block in &order {
            if *block == function.entry {
                continue;
            }
            let mut next = predecessors[block]
                .iter()
                .map(|predecessor| dominators[predecessor].clone())
                .reduce(|left, right| left.intersection(&right).copied().collect())
                .unwrap_or_default();
            next.insert(*block);
            if next != dominators[block] {
                dominators.insert(*block, next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut backedges = successors
        .iter()
        .flat_map(|(source, targets)| targets.iter().map(move |target| (*source, *target)))
        .filter(|(source, target)| dominators[source].contains(target))
        .collect::<Vec<_>>();
    backedges.sort_unstable_by_key(|(source, target)| (source.0, target.0));
    let backedges = backedges.into_iter().collect::<HashSet<_>>();
    let mut indegree = reachable
        .iter()
        .copied()
        .map(|block| (block, 0_usize))
        .collect::<HashMap<_, _>>();
    for block in reachable {
        for successor in &successors[block] {
            if !backedges.contains(&(*block, *successor)) {
                *indegree
                    .get_mut(successor)
                    .expect("reachable successor is in the CFG") += 1;
            }
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(block, degree)| (*degree == 0).then_some(*block))
        .collect::<Vec<_>>();
    ready.sort_unstable_by_key(|block| std::cmp::Reverse(block.0));
    let mut visited = 0;
    while let Some(block) = ready.pop() {
        visited += 1;
        for successor in &successors[&block] {
            if backedges.contains(&(block, *successor)) {
                continue;
            }
            let degree = indegree
                .get_mut(successor)
                .expect("reachable successor is in the CFG");
            *degree -= 1;
            if *degree == 0 {
                ready.push(*successor);
            }
        }
        ready.sort_unstable_by_key(|block| std::cmp::Reverse(block.0));
    }
    if visited != reachable.len() {
        return Err(unsupported(
            &function.name,
            "irreducible executable AARM temporary-subregion CFG",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn temporary_subregion_instruction_is_executable(instruction: &mir::Instruction) -> bool {
    match instruction {
        mir::Instruction::Assign { target, value } => {
            temporary_subregion_place_is_executable(target)
                && temporary_subregion_rvalue_is_executable(value)
        }
        mir::Instruction::AllocateArray {
            destination,
            element_type,
            length,
            ..
        } => {
            matches!(destination, mir::Place::Local(_))
                && temporary_subregion_type_is_executable(element_type)
                && temporary_subregion_operand_is_executable(length)
        }
        mir::Instruction::AllocateObject { destination, .. } => {
            matches!(destination, mir::Place::Local(_))
        }
        mir::Instruction::AllocateStringBuilder {
            destination,
            region: mir::AllocationRegion::Temporary,
            ..
        } => matches!(destination, mir::Place::Local(_)),
        mir::Instruction::AllocateList {
            destination,
            element_type,
            region: mir::AllocationRegion::Temporary,
        } => {
            matches!(destination, mir::Place::Local(_))
                && temporary_subregion_type_is_executable(element_type)
        }
        mir::Instruction::AllocateDictionary {
            destination,
            key_type,
            value_type,
            region: mir::AllocationRegion::Temporary,
        } => {
            matches!(destination, mir::Place::Local(_))
                && temporary_subregion_type_is_executable(key_type)
                && temporary_subregion_type_is_executable(value_type)
        }
        mir::Instruction::StringBuilderAppend { builder, .. } => {
            matches!(builder.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
        }
        mir::Instruction::StringBuilderToString {
            destination,
            builder,
            ..
        } => {
            matches!(destination, mir::Place::Local(_))
                && matches!(builder.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
        }
        mir::Instruction::ListAdd { list, value } => {
            matches!(list.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
                && temporary_subregion_operand_is_executable(value)
        }
        mir::Instruction::ListGet {
            destination,
            list,
            index,
            element_type,
        } => {
            matches!(destination, mir::Place::Local(_))
                && matches!(list.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
                && temporary_subregion_operand_is_executable(index)
                && temporary_subregion_type_is_executable(element_type)
        }
        mir::Instruction::ListRemoveAt { list, index } => {
            matches!(list.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
                && temporary_subregion_operand_is_executable(index)
        }
        mir::Instruction::ListSet { list, index, value } => {
            matches!(list.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
                && temporary_subregion_operand_is_executable(index)
                && temporary_subregion_operand_is_executable(value)
        }
        mir::Instruction::ListClear { list } => {
            matches!(list.kind, mir::OperandKind::Copy(mir::Place::Local(_)))
        }
        mir::Instruction::DictionaryAdd {
            destination,
            dictionary,
            key,
            value,
        }
        | mir::Instruction::DictionarySet {
            destination,
            dictionary,
            key,
            value,
        } => {
            matches!(destination, mir::Place::Local(_))
                && matches!(
                    dictionary.kind,
                    mir::OperandKind::Copy(mir::Place::Local(_))
                )
                && temporary_subregion_operand_is_executable(key)
                && temporary_subregion_operand_is_executable(value)
        }
        mir::Instruction::DictionaryContainsKey {
            destination,
            dictionary,
            key,
        }
        | mir::Instruction::DictionaryRemove {
            destination,
            dictionary,
            key,
        } => {
            matches!(destination, mir::Place::Local(_))
                && matches!(
                    dictionary.kind,
                    mir::OperandKind::Copy(mir::Place::Local(_))
                )
                && temporary_subregion_operand_is_executable(key)
        }
        mir::Instruction::DictionaryTryGet {
            destination,
            dictionary,
            key,
            value_type,
            ..
        } => {
            matches!(destination, mir::Place::Local(_))
                && matches!(
                    dictionary.kind,
                    mir::OperandKind::Copy(mir::Place::Local(_))
                )
                && temporary_subregion_operand_is_executable(key)
                && temporary_subregion_type_is_executable(value_type)
        }
        mir::Instruction::DictionaryClear { dictionary } => matches!(
            dictionary.kind,
            mir::OperandKind::Copy(mir::Place::Local(_))
        ),
        mir::Instruction::CallIntrinsic {
            destination,
            intrinsic,
            arguments,
            return_type,
        } => temporary_subregion_immutable_string_intrinsic_is_executable(
            destination.as_ref(),
            *intrinsic,
            arguments,
            return_type,
        ),
        mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. }
        | mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::Call { .. }
        | mir::Instruction::ForeignCall { .. }
        | mir::Instruction::CallInterface { .. }
        | mir::Instruction::AllocateList {
            region: mir::AllocationRegion::Persistent,
            ..
        }
        | mir::Instruction::AllocateDictionary {
            region: mir::AllocationRegion::Persistent,
            ..
        }
        | mir::Instruction::AllocateStringBuilder {
            region: mir::AllocationRegion::Persistent,
            ..
        }
        | mir::Instruction::DictionaryEntries { .. }
        | mir::Instruction::DictionaryKeys { .. }
        | mir::Instruction::DictionaryValues { .. }
        | mir::Instruction::ListToArray { .. }
        | mir::Instruction::StringDecodeNext { .. } => false,
    }
}

fn temporary_subregion_immutable_string_intrinsic_is_executable(
    destination: Option<&mir::Place>,
    intrinsic: mir::Intrinsic,
    arguments: &[mir::Operand],
    return_type: &mir::Type,
) -> bool {
    matches!(destination, Some(mir::Place::Local(_)))
        && return_type == &mir::Type::String
        && match intrinsic {
            mir::Intrinsic::StringConcatTemporary => {
                matches!(arguments, [left, right] if temporary_subregion_string_input_is_executable(left) && temporary_subregion_string_input_is_executable(right))
            }
            mir::Intrinsic::StringJoinTemporary => arguments
                .iter()
                .all(temporary_subregion_string_input_is_executable),
            mir::Intrinsic::StringSubstringFromTemporary => {
                matches!(arguments, [value, start]
                    if temporary_subregion_string_input_is_executable(value)
                        && start.type_ == mir::Type::Int
                        && temporary_subregion_operand_is_executable(start))
            }
            mir::Intrinsic::StringSubstringRangeTemporary => {
                matches!(arguments, [value, start, length]
                    if temporary_subregion_string_input_is_executable(value)
                        && start.type_ == mir::Type::Int
                        && length.type_ == mir::Type::Int
                        && temporary_subregion_operand_is_executable(start)
                        && temporary_subregion_operand_is_executable(length))
            }
            mir::Intrinsic::StringFromLongTemporary | mir::Intrinsic::StringFromULongTemporary => {
                matches!(arguments, [value]
                if match intrinsic {
                    mir::Intrinsic::StringFromLongTemporary => matches!(
                        value.type_,
                        mir::Type::SByte | mir::Type::Short | mir::Type::Int | mir::Type::Long
                    ),
                    mir::Intrinsic::StringFromULongTemporary => matches!(
                        value.type_,
                        mir::Type::Byte | mir::Type::UShort | mir::Type::UInt | mir::Type::ULong
                    ),
                    _ => false,
                })
            }
            mir::Intrinsic::StringFromDoubleTemporary => {
                matches!(arguments, [value] if value.type_ == mir::Type::Double)
            }
            mir::Intrinsic::StringFromFloatTemporary => {
                matches!(arguments, [value] if value.type_ == mir::Type::Float)
            }
            mir::Intrinsic::StringFromBoolTemporary => {
                matches!(arguments, [value] if value.type_ == mir::Type::Bool)
            }
            mir::Intrinsic::StringFromCharTemporary => {
                matches!(arguments, [value] if value.type_ == mir::Type::Char)
            }
            _ => false,
        }
}

fn temporary_subregion_string_input_is_executable(operand: &mir::Operand) -> bool {
    operand.type_ == mir::Type::String
        && matches!(
            operand.kind,
            mir::OperandKind::Constant(mir::Constant::String(_)) | mir::OperandKind::Copy(_)
        )
}

fn temporary_subregion_rvalue_is_executable(value: &mir::Rvalue) -> bool {
    if !temporary_subregion_type_is_executable(&value.type_) {
        return false;
    }
    match &value.kind {
        mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => {
            temporary_subregion_operand_is_executable(operand)
        }
        mir::RvalueKind::Binary {
            left,
            operator,
            right,
        } => {
            !matches!(
                operator,
                mir::BinaryOperator::Divide | mir::BinaryOperator::Remainder
            ) && temporary_subregion_operand_is_executable(left)
                && temporary_subregion_operand_is_executable(right)
        }
        mir::RvalueKind::Equality { left, right, .. } => {
            temporary_subregion_equality_type_is_executable(&left.type_)
                && temporary_subregion_operand_is_executable(left)
                && temporary_subregion_operand_is_executable(right)
        }
        mir::RvalueKind::ArrayLength(operand) => {
            matches!(operand.type_, mir::Type::Array(_))
                && temporary_subregion_operand_is_executable(operand)
        }
        mir::RvalueKind::Aggregate(_)
        | mir::RvalueKind::EnumConstruct { .. }
        | mir::RvalueKind::Discriminant(_)
        | mir::RvalueKind::ListLength(_)
        | mir::RvalueKind::DictionaryLength(_)
        | mir::RvalueKind::ListVersion(_)
        | mir::RvalueKind::StringByteLength(_)
        | mir::RvalueKind::MakeInterface { .. } => false,
    }
}

fn temporary_subregion_operand_is_executable(operand: &mir::Operand) -> bool {
    temporary_subregion_type_is_executable(&operand.type_)
        && match &operand.kind {
            mir::OperandKind::Constant(mir::Constant::String(_)) => false,
            mir::OperandKind::Constant(_) | mir::OperandKind::Function(_) => true,
            mir::OperandKind::Copy(place) => temporary_subregion_place_is_executable(place),
        }
}

fn temporary_subregion_place_is_executable(place: &mir::Place) -> bool {
    match place {
        mir::Place::Local(_) => true,
        mir::Place::Index {
            array,
            index,
            element_type,
            ..
        } => {
            matches!(
                &array.type_,
                mir::Type::Array(array_element) if array_element.as_ref() == element_type
            ) && temporary_subregion_type_is_executable(element_type)
                && temporary_subregion_operand_is_executable(array)
                && temporary_subregion_operand_is_executable(index)
        }
        mir::Place::ObjectField { object, .. } => {
            matches!(object.type_, mir::Type::Class(_))
                && temporary_subregion_operand_is_executable(object)
        }
        mir::Place::Symbol(_) | mir::Place::Field { .. } | mir::Place::EnumField { .. } => false,
    }
}

fn temporary_subregion_type_is_executable(type_: &mir::Type) -> bool {
    match type_ {
        mir::Type::Array(element) => temporary_subregion_type_is_executable(element),
        mir::Type::Bool
        | mir::Type::SByte
        | mir::Type::Byte
        | mir::Type::Short
        | mir::Type::UShort
        | mir::Type::Int
        | mir::Type::UInt
        | mir::Type::Long
        | mir::Type::ULong
        | mir::Type::Float
        | mir::Type::Double
        | mir::Type::Char
        | mir::Type::Class(_) => true,
        mir::Type::String
        | mir::Type::User(_)
        | mir::Type::Interface(_)
        | mir::Type::Enum(_)
        | mir::Type::Task(_)
        | mir::Type::List(_)
        | mir::Type::Dictionary(_, _)
        | mir::Type::Void
        | mir::Type::Decimal
        | mir::Type::Unknown => false,
    }
}

fn temporary_subregion_equality_type_is_executable(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::Bool
            | mir::Type::SByte
            | mir::Type::Byte
            | mir::Type::Short
            | mir::Type::UShort
            | mir::Type::Int
            | mir::Type::UInt
            | mir::Type::Long
            | mir::Type::ULong
            | mir::Type::Float
            | mir::Type::Double
            | mir::Type::Char
            | mir::Type::Class(_)
            | mir::Type::Array(_)
    )
}

fn is_concurrency_intrinsic(intrinsic: mir::Intrinsic) -> bool {
    matches!(
        intrinsic,
        mir::Intrinsic::TaskRun
            | mir::Intrinsic::TaskWait
            | mir::Intrinsic::AsyncSpawn
            | mir::Intrinsic::AsyncState
            | mir::Intrinsic::AsyncSetState
            | mir::Intrinsic::AsyncStoreSlot
            | mir::Intrinsic::AsyncLoadSlot
            | mir::Intrinsic::AsyncSpawnInner
            | mir::Intrinsic::AsyncAwaitResult
            | mir::Intrinsic::AsyncSetResult
            | mir::Intrinsic::ParallelFor
            | mir::Intrinsic::ParallelForEach
            | mir::Intrinsic::ParallelReduce
    )
}

fn declared_local_type<'a>(
    place: &mir::Place,
    function_name: &str,
    operation: &str,
    locals: &'a HashMap<mir::LocalId, mir::Type>,
) -> Result<&'a mir::Type, BackendError> {
    let mir::Place::Local(local) = place else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `{operation}` destination that is not a local"
        )));
    };
    locals.get(local).ok_or_else(|| {
        BackendError::new(format!(
            "function `{function_name}` has a `{operation}` destination that is undeclared"
        ))
    })
}

fn validate_dictionary_receiver_and_key<'a>(
    dictionary: &'a mir::Operand,
    key: &mir::Operand,
    function_name: &str,
    operation: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(&'a mir::Type, &'a mir::Type), BackendError> {
    validate_operand(dictionary, function_name)?;
    validate_operand(key, function_name)?;
    validate_dictionary_operand_locals(dictionary, operation, function_name, locals)?;
    validate_dictionary_operand_locals(key, operation, function_name, locals)?;
    let mir::Type::Dictionary(key_type, value_type) = &dictionary.type_ else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `{operation}` receiver that is not `Dictionary<K, V>`"
        )));
    };
    validate_dictionary_key_type(key_type, function_name)?;
    if key.type_ != **key_type {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `{operation}` key incompatible with its Dictionary specialization"
        )));
    }
    Ok((key_type, value_type))
}

fn validate_dictionary_mutation(
    destination: &mir::Place,
    dictionary: &mir::Operand,
    key: &mir::Operand,
    value: &mir::Operand,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    let (_, value_type) = validate_dictionary_receiver_and_key(
        dictionary,
        key,
        function_name,
        "Dictionary mutation",
        locals,
    )?;
    validate_operand(value, function_name)?;
    if value.type_ != *value_type {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a Dictionary value incompatible with its specialization"
        )));
    }
    let declared = declared_local_type(destination, function_name, "Dictionary mutation", locals)?;
    if *declared != mir::Type::Bool {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a Dictionary mutation destination that is not `bool`"
        )));
    }
    Ok(())
}

fn validate_dictionary_key_operation(
    destination: &mir::Place,
    dictionary: &mir::Operand,
    key: &mir::Operand,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_dictionary_receiver_and_key(
        dictionary,
        key,
        function_name,
        "Dictionary key operation",
        locals,
    )?;
    let declared = declared_local_type(
        destination,
        function_name,
        "Dictionary key operation",
        locals,
    )?;
    if *declared != mir::Type::Bool {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a Dictionary key-operation destination that is not `bool`"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_dictionary_try_get(
    destination: &mir::Place,
    dictionary: &mir::Operand,
    key: &mir::Operand,
    value_type: &mir::Type,
    option_layout: mir::DictionaryOptionLayout,
    function_name: &str,
    enums: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    let (_, dictionary_value_type) = validate_dictionary_receiver_and_key(
        dictionary,
        key,
        function_name,
        "DictionaryTryGet",
        locals,
    )?;
    if dictionary_value_type != value_type {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `DictionaryTryGet` value type incompatible with its specialization"
        )));
    }
    let declared = declared_local_type(destination, function_name, "DictionaryTryGet", locals)?;
    let mir::Type::Enum(symbol) = declared else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `DictionaryTryGet` destination that is not an enum"
        )));
    };
    if !enums.contains(symbol) || option_layout.some_tag == option_layout.none_tag {
        return Err(BackendError::new(format!(
            "function `{function_name}` has invalid nominal Option metadata for `DictionaryTryGet`"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_dictionary_entries(
    destination: &mir::Place,
    dictionary: &mir::Operand,
    key_type: &mir::Type,
    value_type: &mir::Type,
    entry_type: &mir::Type,
    entry_layout: mir::DictionaryEntryLayout,
    function_name: &str,
    structs: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_operand(dictionary, function_name)?;
    validate_dictionary_operand_locals(dictionary, "DictionaryEntries", function_name, locals)?;
    let expected_dictionary =
        mir::Type::Dictionary(Box::new(key_type.clone()), Box::new(value_type.clone()));
    if dictionary.type_ != expected_dictionary {
        return Err(BackendError::new(format!(
            "function `{function_name}` has `DictionaryEntries` metadata incompatible with its receiver"
        )));
    }
    validate_dictionary_key_type(key_type, function_name)?;
    let mir::Type::User(entry_symbol) = entry_type else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `DictionaryEntries` entry type that is not a concrete struct"
        )));
    };
    if !structs.contains(entry_symbol) || entry_layout.key_field == entry_layout.value_field {
        return Err(BackendError::new(format!(
            "function `{function_name}` has invalid nominal `DictionaryEntry<K, V>` metadata"
        )));
    }
    let declared = declared_local_type(destination, function_name, "DictionaryEntries", locals)?;
    let expected = mir::Type::Array(Box::new(entry_type.clone()));
    if *declared != expected {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `DictionaryEntries` destination with the wrong array element type"
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_dictionary_snapshot(
    destination: &mir::Place,
    dictionary: &mir::Operand,
    component_type: &mir::Type,
    keys: bool,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_operand(dictionary, function_name)?;
    validate_dictionary_operand_locals(dictionary, "Dictionary snapshot", function_name, locals)?;
    let mir::Type::Dictionary(key_type, value_type) = &dictionary.type_ else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a Dictionary snapshot receiver that is not `Dictionary<K, V>`"
        )));
    };
    let expected_component = if keys {
        key_type.as_ref()
    } else {
        value_type.as_ref()
    };
    if component_type != expected_component {
        return Err(BackendError::new(format!(
            "function `{function_name}` has Dictionary snapshot metadata incompatible with its receiver"
        )));
    }
    validate_list_element_type(
        component_type,
        function_name,
        classes,
        structs,
        interfaces,
        enums,
    )?;
    let declared = declared_local_type(destination, function_name, "Dictionary snapshot", locals)?;
    if declared != &mir::Type::Array(Box::new(component_type.clone())) {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a Dictionary snapshot destination with the wrong array element type"
        )));
    }
    Ok(())
}

fn validate_dictionary_operand_locals(
    operand: &mir::Operand,
    operation: &str,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    let mir::OperandKind::Copy(place) = &operand.kind else {
        return Ok(());
    };
    validate_dictionary_place_locals(place, operation, function_name, locals)?;
    if let mir::Place::Local(local) = place {
        let declared = &locals[local];
        if declared != &operand.type_ {
            return Err(BackendError::new(format!(
                "function `{function_name}` passes a `{operation}` operand declared `{declared:?}` but typed `{:?}`",
                operand.type_
            )));
        }
    }
    Ok(())
}

fn validate_dictionary_place_locals(
    place: &mir::Place,
    operation: &str,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    match place {
        mir::Place::Local(local) => {
            if locals.contains_key(local) {
                Ok(())
            } else {
                Err(BackendError::new(format!(
                    "function `{function_name}` passes undeclared local {local:?} to {operation}"
                )))
            }
        }
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            validate_dictionary_place_locals(base, operation, function_name, locals)
        }
        mir::Place::Index { array, index, .. } => {
            validate_dictionary_operand_locals(array, operation, function_name, locals)?;
            validate_dictionary_operand_locals(index, operation, function_name, locals)
        }
        mir::Place::ObjectField { object, .. } => {
            validate_dictionary_operand_locals(object, operation, function_name, locals)
        }
        mir::Place::Symbol(_) => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_string_decode_next(
    string: &mir::Operand,
    cursor: &mir::Operand,
    char_destination: &mir::Place,
    next_cursor_destination: &mir::Place,
    ok_destination: &mir::Place,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_operand(string, function_name)?;
    validate_operand(cursor, function_name)?;
    validate_place(char_destination, function_name)?;
    validate_place(next_cursor_destination, function_name)?;
    validate_place(ok_destination, function_name)?;
    if string.type_ != mir::Type::String {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `StringDecodeNext` on a non-`string` receiver (found `{}`)",
            type_name_owned(&string.type_)
        )));
    }
    if cursor.type_ != mir::Type::Int {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `StringDecodeNext` whose cursor is not `int` (found `{}`)",
            type_name_owned(&cursor.type_)
        )));
    }
    let destinations = [
        (char_destination, mir::Type::Char, "char_destination"),
        (
            next_cursor_destination,
            mir::Type::Int,
            "next_cursor_destination",
        ),
        (ok_destination, mir::Type::Bool, "ok_destination"),
    ];
    let mut seen_locals = HashSet::new();
    for (place, expected_type, label) in destinations {
        let mir::Place::Local(local) = place else {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a `StringDecodeNext` whose `{label}` is not a local"
            )));
        };
        let declared = locals.get(local).ok_or_else(|| {
            BackendError::new(format!(
                "function `{function_name}` has a `StringDecodeNext` writing `{label}` into an undeclared local"
            ))
        })?;
        if *declared != expected_type {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a `StringDecodeNext` whose `{label}` is declared `{}`, expected `{}`",
                type_name_owned(declared),
                type_name_owned(&expected_type),
            )));
        }
        if !seen_locals.insert(*local) {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a `StringDecodeNext` writing more than one destination into the same local"
            )));
        }
    }
    Ok(())
}

fn validate_allocate_array(
    destination: &mir::Place,
    element_type: &mir::Type,
    length: &mir::Operand,
    initialization: mir::ArrayInitialization,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_place(destination, function_name)?;
    validate_value_type(element_type, function_name)?;
    validate_operand(length, function_name)?;
    let mir::Place::Local(destination_local) = destination else {
        return Err(BackendError::new(format!(
            "function `{function_name}` allocates an array into a non-local destination"
        )));
    };
    let declared_destination = locals.get(destination_local).ok_or_else(|| {
        BackendError::new(format!(
            "function `{function_name}` allocates an array into an undeclared local"
        ))
    })?;
    let expected_destination = mir::Type::Array(Box::new(element_type.clone()));
    if *declared_destination != expected_destination {
        return Err(BackendError::new(format!(
            "function `{function_name}` allocates `{}` elements into destination `{}`",
            type_name(element_type),
            type_name(declared_destination)
        )));
    }
    if length.type_ != mir::Type::Int {
        return Err(BackendError::new(format!(
            "function `{function_name}` allocates an array with non-`int` length `{}`",
            type_name(&length.type_)
        )));
    }
    if initialization == mir::ArrayInitialization::Empty
        && !matches!(
            &length.kind,
            mir::OperandKind::Constant(mir::Constant::Integer(value))
                if matches!(integer_constant_bits(value, &length.type_), Ok(0))
        )
    {
        return Err(BackendError::new(format!(
            "function `{function_name}` marks a non-zero or dynamic array allocation as proven empty"
        )));
    }
    Ok(())
}

fn constant_nonnegative_int(operand: &mir::Operand) -> Option<usize> {
    let mir::Operand {
        type_: mir::Type::Int,
        kind: mir::OperandKind::Constant(mir::Constant::Integer(value)),
    } = operand
    else {
        return None;
    };
    usize::try_from(value.parse::<i32>().ok()?).ok()
}

fn explicit_array_element_assignment(
    instruction: &mir::Instruction,
    array_local: mir::LocalId,
    element_type: &mir::Type,
) -> Option<usize> {
    let mir::Instruction::Assign {
        target:
            mir::Place::Index {
                array,
                index,
                element_type: target_element,
                ..
            },
        value,
    } = instruction
    else {
        return None;
    };
    if target_element != element_type || value.type_ != *element_type {
        return None;
    }
    if !matches!(
        array.kind,
        mir::OperandKind::Copy(mir::Place::Local(local)) if local == array_local
    ) {
        return None;
    }
    let mut reads = HashSet::new();
    owned_rvalue_reads(value, &mut reads);
    if reads.contains(&array_local) {
        return None;
    }
    constant_nonnegative_int(index)
}

fn merge_explicit_array_state(
    incoming: &mut HashMap<mir::BasicBlockId, HashSet<usize>>,
    pending: &mut Vec<mir::BasicBlockId>,
    block: mir::BasicBlockId,
    state: &HashSet<usize>,
) {
    if let Some(current) = incoming.get_mut(&block) {
        let merged = current.intersection(state).copied().collect::<HashSet<_>>();
        if merged != *current {
            *current = merged;
            pending.push(block);
        }
    } else {
        incoming.insert(block, state.clone());
        pending.push(block);
    }
}

fn validate_one_explicit_array_initialization(
    function: &mir::Function,
    allocation_block: mir::BasicBlockId,
    allocation_index: usize,
    array_local: mir::LocalId,
    element_type: &mir::Type,
    element_count: usize,
) -> Result<(), BackendError> {
    if element_count == 0 {
        return Ok(());
    }
    let blocks = function
        .blocks
        .iter()
        .map(|block| (block.id, block))
        .collect::<HashMap<_, _>>();
    let mut incoming = HashMap::new();
    let mut pending = vec![allocation_block];
    incoming.insert(allocation_block, HashSet::new());
    let mut first_visit = true;

    while let Some(block_id) = pending.pop() {
        let block = blocks.get(&block_id).ok_or_else(|| {
            BackendError::new(format!(
                "function `{}` has malformed explicit array initialization CFG",
                function.name
            ))
        })?;
        let mut initialized = incoming.get(&block_id).cloned().ok_or_else(|| {
            BackendError::new(format!(
                "function `{}` has missing explicit array initialization state",
                function.name
            ))
        })?;
        let start = if first_visit && block_id == allocation_block {
            first_visit = false;
            allocation_index + 1
        } else {
            0
        };
        let mut complete = false;
        for instruction in block.instructions.iter().skip(start) {
            if let Some(index) =
                explicit_array_element_assignment(instruction, array_local, element_type)
            {
                if index >= element_count {
                    return Err(BackendError::new(format!(
                        "function `{}` initializes explicit array element {index} outside length {element_count}",
                        function.name
                    )));
                }
                initialized.insert(index);
                if initialized.len() == element_count {
                    complete = true;
                    break;
                }
                continue;
            }

            let mut reads = HashSet::new();
            owned_instruction_reads(instruction, &mut reads);
            let mut definitions = HashSet::new();
            owned_instruction_definitions(instruction, &mut definitions);
            if reads.contains(&array_local) || definitions.contains(&array_local) {
                return Err(BackendError::new(format!(
                    "function `{}` uses or replaces an explicitly initialized array before every element is written",
                    function.name
                )));
            }
        }
        if complete {
            continue;
        }

        let mut terminator_reads = HashSet::new();
        match &block.terminator {
            mir::Terminator::Branch { condition, .. }
            | mir::Terminator::Return(Some(condition)) => {
                owned_operand_reads(condition, &mut terminator_reads);
            }
            _ => {}
        }
        if terminator_reads.contains(&array_local) {
            return Err(BackendError::new(format!(
                "function `{}` uses an explicitly initialized array in control flow before every element is written",
                function.name
            )));
        }
        let successors = match block.terminator {
            mir::Terminator::Goto(target) => vec![target],
            mir::Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![then_block, else_block],
            mir::Terminator::Return(_) | mir::Terminator::End | mir::Terminator::Unreachable => {
                return Err(BackendError::new(format!(
                    "function `{}` terminates before every explicit array element is initialized",
                    function.name
                )));
            }
        };
        for successor in successors {
            merge_explicit_array_state(&mut incoming, &mut pending, successor, &initialized);
        }
    }
    Ok(())
}

fn validate_explicit_array_initialization(function: &mir::Function) -> Result<(), BackendError> {
    for block in &function.blocks {
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            let mir::Instruction::AllocateArray {
                destination: mir::Place::Local(array_local),
                element_type,
                length,
                initialization: mir::ArrayInitialization::Explicit,
                ..
            } = instruction
            else {
                continue;
            };
            let element_count = constant_nonnegative_int(length).ok_or_else(|| {
                BackendError::new(format!(
                    "function `{}` has explicit array initialization without a constant non-negative length",
                    function.name
                ))
            })?;
            validate_one_explicit_array_initialization(
                function,
                block.id,
                instruction_index,
                *array_local,
                element_type,
                element_count,
            )?;
        }
    }
    Ok(())
}

fn validate_assign(
    target: &mir::Place,
    value: &mir::Rvalue,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
    implementations: &HashSet<(mir::SymbolId, mir::SymbolId)>,
) -> Result<(), BackendError> {
    validate_place_with_proven_bounds(target, function_name, true)?;
    if let mir::Place::Local(id) = target {
        let declared = locals.get(id).ok_or_else(|| {
            BackendError::new(format!(
                "function `{function_name}` assigns into undeclared local {id:?}"
            ))
        })?;
        if *declared != value.type_ {
            return Err(BackendError::new(format!(
                "function `{function_name}` assigns a value of type `{}` into a local declared `{}`",
                type_name(&value.type_),
                type_name(declared)
            )));
        }
    }
    validate_rvalue(value, function_name, implementations, locals)
}

#[allow(clippy::too_many_arguments)]
fn validate_call(
    destination: Option<&mir::Place>,
    function: mir::SymbolId,
    arguments: &[mir::Operand],
    return_type: &mir::Type,
    function_name: &str,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
) -> Result<(), BackendError> {
    if let Some(destination) = destination {
        validate_place(destination, function_name)?;
    }
    validate_return_type(return_type, function_name)?;
    for argument in arguments {
        validate_operand(argument, function_name)?;
    }
    let called = signatures.get(&function).ok_or_else(|| {
        BackendError::new(format!(
            "function `{function_name}` calls an unsupported external function with symbol {}",
            function.0
        ))
    })?;
    if called
        .owner
        .is_some_and(|owner| !classes.contains(&owner) && !structs.contains(&owner))
    {
        return Err(BackendError::new(format!(
            "function `{function_name}` calls a method with an unknown owner"
        )));
    }
    if called.return_type != *return_type {
        return Err(BackendError::new(format!(
            "function `{function_name}` calls `{}` with return type `{}`, but the declaration returns `{}`",
            called.name,
            type_name(return_type),
            type_name(&called.return_type)
        )));
    }
    if arguments.len() != called.parameters.len()
        || arguments
            .iter()
            .zip(&called.parameters)
            .any(|(argument, parameter)| argument.type_ != parameter.type_)
    {
        return Err(BackendError::new(format!(
            "function `{function_name}` calls `{}` with an invalid argument signature",
            called.name
        )));
    }
    Ok(())
}

fn validate_allocate_object(
    destination: &mir::Place,
    class: mir::SymbolId,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
) -> Result<(), BackendError> {
    validate_place(destination, function_name)?;
    if classes.contains(&class) {
        Ok(())
    } else {
        Err(unsupported(function_name, "allocation of a non-class type"))
    }
}

fn validate_call_intrinsic(
    destination: Option<&mir::Place>,
    intrinsic: mir::Intrinsic,
    arguments: &[mir::Operand],
    return_type: &mir::Type,
    function_name: &str,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    if let Some(destination) = destination {
        validate_place(destination, function_name)?;
        if is_string_method_intrinsic(intrinsic) || is_math_intrinsic(intrinsic) {
            let mir::Place::Local(local) = destination else {
                return Err(BackendError::new(format!(
                    "function `{function_name}` stores {intrinsic:?} into a non-local destination"
                )));
            };
            let declared = locals.get(local).ok_or_else(|| {
                BackendError::new(format!(
                    "function `{function_name}` stores {intrinsic:?} into undeclared local {local:?}"
                ))
            })?;
            if declared != return_type {
                return Err(BackendError::new(format!(
                    "function `{function_name}` stores {intrinsic:?} result expected `{return_type:?}`, found destination `{declared:?}`"
                )));
            }
        }
    }
    validate_return_type(return_type, function_name)?;
    for argument in arguments {
        // Spawn-style intrinsics (`Task.Run`, `AsyncSpawn`, `AsyncSpawnInner`,
        // `Parallel*`) carry a resolved function reference as an
        // `OperandKind::Function`, which the generic `validate_operand`
        // rejects. Validate only its value type; `validate_intrinsic_shape`
        // below checks the full shape.
        if matches!(argument.kind, mir::OperandKind::Function(_)) {
            validate_value_type(&argument.type_, function_name)?;
        } else {
            validate_operand(argument, function_name)?;
            if is_string_method_intrinsic(intrinsic) {
                validate_string_operand_locals(argument, intrinsic, function_name, locals)?;
            }
        }
    }
    validate_intrinsic_shape(
        destination,
        intrinsic,
        arguments,
        return_type,
        function_name,
        signatures,
    )
}

fn is_string_method_intrinsic(intrinsic: mir::Intrinsic) -> bool {
    matches!(
        intrinsic,
        mir::Intrinsic::StringContains
            | mir::Intrinsic::StringStartsWith
            | mir::Intrinsic::StringEndsWith
            | mir::Intrinsic::StringIndexOf
            | mir::Intrinsic::StringSubstringFrom
            | mir::Intrinsic::StringSubstringFromTemporary
            | mir::Intrinsic::StringSubstringRange
            | mir::Intrinsic::StringSubstringRangeTemporary
            | mir::Intrinsic::StringTrim
            | mir::Intrinsic::StringTrimTemporary
            | mir::Intrinsic::StringReplace
            | mir::Intrinsic::StringReplaceTemporary
            | mir::Intrinsic::StringSplit
            | mir::Intrinsic::StringSplitTemporary
            | mir::Intrinsic::StringTryParseBool
            | mir::Intrinsic::StringTryParseInt
            | mir::Intrinsic::StringTryParseUInt
            | mir::Intrinsic::StringTryParseLong
            | mir::Intrinsic::StringTryParseULong
            | mir::Intrinsic::StringTryParseFloat
            | mir::Intrinsic::StringTryParseDouble
            | mir::Intrinsic::ConsoleWrite
            | mir::Intrinsic::ConsoleWriteLine
            | mir::Intrinsic::ConsoleReadLine
            | mir::Intrinsic::ConsoleReadLineTemporary
    )
}

fn is_math_intrinsic(intrinsic: mir::Intrinsic) -> bool {
    matches!(
        intrinsic,
        mir::Intrinsic::MathUnaryFloat
            | mir::Intrinsic::MathUnaryDouble
            | mir::Intrinsic::MathPowFloat
            | mir::Intrinsic::MathPowDouble
    )
}

fn validate_string_operand_locals(
    operand: &mir::Operand,
    intrinsic: mir::Intrinsic,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    let mir::OperandKind::Copy(place) = &operand.kind else {
        return Ok(());
    };
    validate_string_place_locals(place, intrinsic, function_name, locals)?;
    if let mir::Place::Local(local) = place {
        let declared = &locals[local];
        if declared != &operand.type_ {
            return Err(BackendError::new(format!(
                "function `{function_name}` passes {intrinsic:?} operand expected `{:?}`, found local `{declared:?}`",
                operand.type_
            )));
        }
    }
    Ok(())
}

fn validate_string_place_locals(
    place: &mir::Place,
    intrinsic: mir::Intrinsic,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    match place {
        mir::Place::Local(local) => {
            if locals.contains_key(local) {
                Ok(())
            } else {
                Err(BackendError::new(format!(
                    "function `{function_name}` passes undeclared local {local:?} to {intrinsic:?}"
                )))
            }
        }
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            validate_string_place_locals(base, intrinsic, function_name, locals)
        }
        mir::Place::Index { array, index, .. } => {
            validate_string_operand_locals(array, intrinsic, function_name, locals)?;
            validate_string_operand_locals(index, intrinsic, function_name, locals)
        }
        mir::Place::ObjectField { object, .. } => {
            validate_string_operand_locals(object, intrinsic, function_name, locals)
        }
        mir::Place::Symbol(_) => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_allocate_list(
    destination: &mir::Place,
    element_type: &mir::Type,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_place(destination, function_name)?;
    validate_list_element_type(
        element_type,
        function_name,
        classes,
        structs,
        interfaces,
        enums,
    )?;
    if let mir::Place::Local(local) = destination {
        let declared = locals.get(local).ok_or_else(|| {
            BackendError::new(format!(
                "function `{function_name}` allocates a `List<T>` into an undeclared local"
            ))
        })?;
        let expected = mir::Type::List(Box::new(element_type.clone()));
        if *declared != expected {
            return Err(BackendError::new(format!(
                "function `{function_name}` has an `AllocateList` whose destination is declared `{}`, but the instruction constructs `{}`",
                type_name_owned(declared),
                type_name_owned(&expected),
            )));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_allocate_dictionary(
    destination: &mir::Place,
    key_type: &mir::Type,
    value_type: &mir::Type,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_place(destination, function_name)?;
    validate_dictionary_key_type(key_type, function_name)?;
    validate_dictionary_value_type(
        value_type,
        function_name,
        classes,
        structs,
        interfaces,
        enums,
    )?;
    if let mir::Place::Local(local) = destination {
        let declared = locals.get(local).ok_or_else(|| {
            BackendError::new(format!(
                "function `{function_name}` allocates a `Dictionary<K, V>` into an undeclared local"
            ))
        })?;
        let expected =
            mir::Type::Dictionary(Box::new(key_type.clone()), Box::new(value_type.clone()));
        if *declared != expected {
            return Err(BackendError::new(format!(
                "function `{function_name}` has an `AllocateDictionary` whose destination type does not match its concrete specialization"
            )));
        }
    }
    Ok(())
}

fn validate_allocate_string_builder(
    destination: &mir::Place,
    class: mir::SymbolId,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_place(destination, function_name)?;
    if !classes.contains(&class) {
        return Err(BackendError::new(format!(
            "function `{function_name}` allocates a StringBuilder with an unknown class identity"
        )));
    }
    let mir::Place::Local(local) = destination else {
        return Err(BackendError::new(format!(
            "function `{function_name}` allocates a StringBuilder into a non-local destination"
        )));
    };
    if locals.get(local) != Some(&mir::Type::Class(class)) {
        return Err(BackendError::new(format!(
            "function `{function_name}` allocates a StringBuilder into an incompatible destination"
        )));
    }
    Ok(())
}

fn validate_string_builder_append(
    builder: &mir::Operand,
    value: &mir::Operand,
    class: mir::SymbolId,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    if !classes.contains(&class)
        || builder.type_ != mir::Type::Class(class)
        || value.type_ != mir::Type::String
        || !matches!(builder.kind, mir::OperandKind::Copy(_))
        || matches!(value.kind, mir::OperandKind::Constant(ref constant) if !matches!(constant, mir::Constant::String(_)))
    {
        return Err(BackendError::new(format!(
            "function `{function_name}` has an invalid StringBuilder.Append operation"
        )));
    }
    validate_operand(builder, function_name)?;
    validate_operand(value, function_name)?;
    validate_builder_operand_local(builder, function_name, locals)?;
    validate_builder_operand_local(value, function_name, locals)
}

fn validate_string_builder_to_string(
    destination: &mir::Place,
    builder: &mir::Operand,
    class: mir::SymbolId,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_place(destination, function_name)?;
    validate_operand(builder, function_name)?;
    if !classes.contains(&class)
        || builder.type_ != mir::Type::Class(class)
        || !matches!(builder.kind, mir::OperandKind::Copy(_))
    {
        return Err(BackendError::new(format!(
            "function `{function_name}` has an invalid StringBuilder.ToString receiver"
        )));
    }
    validate_builder_operand_local(builder, function_name, locals)?;
    let mir::Place::Local(local) = destination else {
        return Err(BackendError::new(format!(
            "function `{function_name}` writes StringBuilder.ToString into a non-local destination"
        )));
    };
    if locals.get(local) != Some(&mir::Type::String) {
        return Err(BackendError::new(format!(
            "function `{function_name}` writes StringBuilder.ToString into a non-string destination"
        )));
    }
    Ok(())
}

fn validate_builder_operand_local(
    operand: &mir::Operand,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    let mir::OperandKind::Copy(mir::Place::Local(local)) = &operand.kind else {
        return Ok(());
    };
    if locals.get(local) != Some(&operand.type_) {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a StringBuilder operation whose operand type does not match its local"
        )));
    }
    Ok(())
}

fn validate_dictionary_key_type(key: &mir::Type, function_name: &str) -> Result<(), BackendError> {
    if matches!(
        key,
        mir::Type::Bool
            | mir::Type::Char
            | mir::Type::SByte
            | mir::Type::Byte
            | mir::Type::Short
            | mir::Type::UShort
            | mir::Type::Int
            | mir::Type::UInt
            | mir::Type::Long
            | mir::Type::ULong
            | mir::Type::String
    ) {
        Ok(())
    } else {
        Err(BackendError::new(format!(
            "function `{function_name}` has a Dictionary key type unsupported in ASTER 1.0: `{}`",
            type_name_owned(key)
        )))
    }
}

fn validate_dictionary_value_type(
    value: &mir::Type,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
) -> Result<(), BackendError> {
    match value {
        mir::Type::User(symbol) if !structs.contains(symbol) => {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a Dictionary value struct unknown to the module"
            )));
        }
        mir::Type::Class(symbol) if !classes.contains(symbol) => {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a Dictionary value class unknown to the module"
            )));
        }
        mir::Type::Interface(symbol) if !interfaces.contains(symbol) => {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a Dictionary value interface unknown to the module"
            )));
        }
        mir::Type::Enum(symbol) if !enums.contains(symbol) => {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a Dictionary value enum unknown to the module"
            )));
        }
        mir::Type::Dictionary(key, nested) => {
            validate_dictionary_key_type(key, function_name)?;
            return validate_dictionary_value_type(
                nested,
                function_name,
                classes,
                structs,
                interfaces,
                enums,
            );
        }
        mir::Type::List(element) => {
            return validate_list_element_type(
                element,
                function_name,
                classes,
                structs,
                interfaces,
                enums,
            );
        }
        _ => {}
    }
    validate_value_type(value, function_name)
}

#[allow(clippy::too_many_arguments)]
fn validate_list_get(
    destination: &mir::Place,
    list: &mir::Operand,
    index: &mir::Operand,
    element_type: &mir::Type,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_place(destination, function_name)?;
    validate_operand(list, function_name)?;
    validate_operand(index, function_name)?;
    if index.type_ != mir::Type::Int {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `ListGet` whose index is not `int` (found `{}`)",
            type_name_owned(&index.type_)
        )));
    }
    let expected_list_type = mir::Type::List(Box::new(element_type.clone()));
    if list.type_ != expected_list_type {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `ListGet` on `{}`, but the instruction constructs `{}`",
            type_name_owned(&list.type_),
            type_name_owned(&expected_list_type),
        )));
    }
    validate_list_element_type(
        element_type,
        function_name,
        classes,
        structs,
        interfaces,
        enums,
    )?;
    if let mir::Place::Local(local) = destination {
        let declared = locals.get(local).ok_or_else(|| {
            BackendError::new(format!(
                "function `{function_name}` has a `ListGet` writing into an undeclared local"
            ))
        })?;
        if *declared != *element_type {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a `ListGet` whose destination is declared `{}`, but the instruction produces `{}`",
                type_name_owned(declared),
                type_name_owned(element_type),
            )));
        }
    }
    Ok(())
}

fn validate_list_add(
    list: &mir::Operand,
    value: &mir::Operand,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
) -> Result<(), BackendError> {
    validate_operand(list, function_name)?;
    validate_operand(value, function_name)?;
    let mir::Type::List(element_type) = &list.type_ else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `ListAdd` whose receiver is not `List<T>` (found `{}`)",
            type_name_owned(&list.type_)
        )));
    };
    validate_list_element_type(
        element_type,
        function_name,
        classes,
        structs,
        interfaces,
        enums,
    )?;
    if value.type_ != **element_type {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `ListAdd` on `{}` receiving a value of type `{}`",
            type_name_owned(&list.type_),
            type_name_owned(&value.type_),
        )));
    }
    Ok(())
}

fn validate_list_remove_at(
    list: &mir::Operand,
    index: &mir::Operand,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
) -> Result<(), BackendError> {
    validate_operand(list, function_name)?;
    validate_operand(index, function_name)?;
    if index.type_ != mir::Type::Int {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `ListRemoveAt` whose index is not `int` (found `{}`)",
            type_name_owned(&index.type_)
        )));
    }
    let mir::Type::List(element_type) = &list.type_ else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `ListRemoveAt` whose receiver is not `List<T>` (found `{}`)",
            type_name_owned(&list.type_)
        )));
    };
    validate_list_element_type(
        element_type,
        function_name,
        classes,
        structs,
        interfaces,
        enums,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_list_set(
    list: &mir::Operand,
    index: &mir::Operand,
    value: &mir::Operand,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_list_add(
        list,
        value,
        function_name,
        classes,
        structs,
        interfaces,
        enums,
    )?;
    if index.type_ != mir::Type::Int {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `ListSet` whose index is not `int`"
        )));
    }
    validate_operand(index, function_name)?;
    validate_direct_operand_local(list, "ListSet receiver", function_name, locals)?;
    validate_direct_operand_local(index, "ListSet index", function_name, locals)?;
    validate_direct_operand_local(value, "ListSet value", function_name, locals)
}

fn validate_list_clear(
    list: &mir::Operand,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_operand(list, function_name)?;
    let mir::Type::List(element_type) = &list.type_ else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `ListClear` whose receiver is not `List<T>`"
        )));
    };
    validate_list_element_type(
        element_type,
        function_name,
        classes,
        structs,
        interfaces,
        enums,
    )?;
    validate_direct_operand_local(list, "ListClear receiver", function_name, locals)
}

#[allow(clippy::too_many_arguments)]
fn validate_list_to_array(
    destination: &mir::Place,
    list: &mir::Operand,
    element_type: &mir::Type,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_operand(list, function_name)?;
    validate_direct_operand_local(list, "ListToArray receiver", function_name, locals)?;
    let mir::Type::List(receiver_element) = &list.type_ else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `ListToArray` whose receiver is not `List<T>`"
        )));
    };
    if receiver_element.as_ref() != element_type {
        return Err(BackendError::new(format!(
            "function `{function_name}` has `ListToArray` metadata incompatible with its receiver"
        )));
    }
    validate_list_element_type(
        element_type,
        function_name,
        classes,
        structs,
        interfaces,
        enums,
    )?;
    let declared = declared_local_type(destination, function_name, "ListToArray", locals)?;
    if declared != &mir::Type::Array(Box::new(element_type.clone())) {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `ListToArray` destination with the wrong array element type"
        )));
    }
    Ok(())
}

fn validate_direct_operand_local(
    operand: &mir::Operand,
    operation: &str,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    let mir::OperandKind::Copy(mir::Place::Local(local)) = &operand.kind else {
        return Ok(());
    };
    if locals.get(local) == Some(&operand.type_) {
        Ok(())
    } else {
        Err(BackendError::new(format!(
            "function `{function_name}` has a {operation} whose operand type does not match its local"
        )))
    }
}

fn validate_dictionary_clear(
    dictionary: &mir::Operand,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_operand(dictionary, function_name)?;
    validate_dictionary_operand_locals(dictionary, "DictionaryClear", function_name, locals)
}

/// Whether `element_type` is a concrete type `List<T>` may hold: known to
/// the module (any nominal symbol actually declared), not `void`, not
/// `decimal` (checked by the compiler but not executable yet — see
/// `validate_value_type`), and, when it is itself `List<U>`, recursively
/// valid. Reuses `validate_value_type`'s existing rules instead of a second,
/// divergent list of executable types.
fn validate_list_element_type(
    element_type: &mir::Type,
    function_name: &str,
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
) -> Result<(), BackendError> {
    if *element_type == mir::Type::Void {
        return Err(BackendError::new(format!(
            "function `{function_name}` has a `List<void>`, which is not a value type"
        )));
    }
    match element_type {
        mir::Type::User(symbol) if !structs.contains(symbol) => {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a `List<T>` whose element struct is unknown"
            )));
        }
        mir::Type::Class(symbol) if !classes.contains(symbol) => {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a `List<T>` whose element class is unknown"
            )));
        }
        mir::Type::Interface(symbol) if !interfaces.contains(symbol) => {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a `List<T>` whose element interface is unknown"
            )));
        }
        mir::Type::Enum(symbol) if !enums.contains(symbol) => {
            return Err(BackendError::new(format!(
                "function `{function_name}` has a `List<T>` whose element enum is unknown"
            )));
        }
        mir::Type::List(inner) => {
            return validate_list_element_type(
                inner,
                function_name,
                classes,
                structs,
                interfaces,
                enums,
            );
        }
        _ => {}
    }
    validate_value_type(element_type, function_name)
}

fn type_name_owned(type_: &mir::Type) -> String {
    match type_ {
        mir::Type::List(element) => format!("List<{}>", type_name_owned(element)),
        mir::Type::Dictionary(key, value) => format!(
            "Dictionary<{}, {}>",
            type_name_owned(key),
            type_name_owned(value)
        ),
        mir::Type::Array(element) => format!("{}[]", type_name_owned(element)),
        mir::Type::Task(result) => format!("Task<{}>", type_name_owned(result)),
        other => type_name(other).to_owned(),
    }
}

fn validate_interface_call(
    destination: Option<&mir::Place>,
    receiver: &mir::Operand,
    arguments: &[mir::Operand],
    return_type: &mir::Type,
    method: mir::SymbolId,
    function_name: &str,
    interface_methods: &InterfaceMethods<'_>,
) -> Result<(), BackendError> {
    if let Some(destination) = destination {
        validate_place(destination, function_name)?;
    }
    let mir::Type::Interface(receiver_interface) = receiver.type_ else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has an interface call with a non-interface receiver"
        )));
    };
    validate_operand(receiver, function_name)?;
    let (method_interface, definition) = interface_methods
        .get(&method)
        .ok_or_else(|| BackendError::new("interface call references an unknown contract method"))?;
    let incompatible = *method_interface != receiver_interface
        || definition.return_type != *return_type
        || definition.parameters.len() != arguments.len()
        || definition
            .parameters
            .iter()
            .zip(arguments)
            .any(|(expected, actual)| expected != &actual.type_);
    if incompatible {
        return Err(BackendError::new(format!(
            "function `{function_name}` contains an interface call with an incompatible signature"
        )));
    }
    for argument in arguments {
        validate_operand(argument, function_name)?;
    }
    validate_return_type(return_type, function_name)
}

#[allow(clippy::too_many_lines)]
// `#[inline(never)]`: this function has a 600-line match that, when inlined
// into `validate_call_intrinsic`, inflates the combined stack frame beyond
// Windows' default 1 MB test-thread limit in debug builds. Keeping it as a
// separate frame has no runtime cost in optimized builds.
#[inline(never)]
fn validate_intrinsic_shape(
    destination: Option<&mir::Place>,
    intrinsic: mir::Intrinsic,
    arguments: &[mir::Operand],
    return_type: &mir::Type,
    function_name: &str,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
) -> Result<(), BackendError> {
    let valid = match intrinsic {
        mir::Intrinsic::Log | mir::Intrinsic::LogWarning | mir::Intrinsic::LogError => {
            destination.is_none()
                && return_type == &mir::Type::Void
                && matches!(arguments, [argument] if argument.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringEquals => {
            destination.is_some()
                && return_type == &mir::Type::Bool
                && matches!(arguments, [left, right] if left.type_ == mir::Type::String && right.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringConcat | mir::Intrinsic::StringConcatTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(arguments, [left, right] if left.type_ == mir::Type::String && right.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringLength => {
            destination.is_some()
                && return_type == &mir::Type::Int
                && matches!(arguments, [value] if value.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringContains
        | mir::Intrinsic::StringStartsWith
        | mir::Intrinsic::StringEndsWith => {
            destination.is_some()
                && return_type == &mir::Type::Bool
                && matches!(arguments, [receiver, value] if receiver.type_ == mir::Type::String && value.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringIndexOf => {
            destination.is_some()
                && return_type == &mir::Type::Int
                && matches!(arguments, [receiver, value] if receiver.type_ == mir::Type::String && value.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringSubstringFrom | mir::Intrinsic::StringSubstringFromTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(arguments, [receiver, start] if receiver.type_ == mir::Type::String && start.type_ == mir::Type::Int)
        }
        mir::Intrinsic::StringSubstringRange | mir::Intrinsic::StringSubstringRangeTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(arguments, [receiver, start, length] if receiver.type_ == mir::Type::String && start.type_ == mir::Type::Int && length.type_ == mir::Type::Int)
        }
        mir::Intrinsic::StringTrim | mir::Intrinsic::StringTrimTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(arguments, [value] if value.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringReplace | mir::Intrinsic::StringReplaceTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(arguments, [value, old_value, new_value] if value.type_ == mir::Type::String && old_value.type_ == mir::Type::String && new_value.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringSplit | mir::Intrinsic::StringSplitTemporary => {
            destination.is_some()
                && return_type == &mir::Type::Array(Box::new(mir::Type::String))
                && matches!(arguments, [value, separator] if value.type_ == mir::Type::String && separator.type_ == mir::Type::String)
        }
        mir::Intrinsic::MathUnaryFloat => {
            destination.is_some()
                && return_type == &mir::Type::Float
                && matches!(arguments, [value, operation] if value.type_ == mir::Type::Float && is_math_unary_operation(operation))
        }
        mir::Intrinsic::MathUnaryDouble => {
            destination.is_some()
                && return_type == &mir::Type::Double
                && matches!(arguments, [value, operation] if value.type_ == mir::Type::Double && is_math_unary_operation(operation))
        }
        mir::Intrinsic::MathPowFloat => {
            destination.is_some()
                && return_type == &mir::Type::Float
                && matches!(arguments, [value, exponent] if value.type_ == mir::Type::Float && exponent.type_ == mir::Type::Float)
        }
        mir::Intrinsic::MathPowDouble => {
            destination.is_some()
                && return_type == &mir::Type::Double
                && matches!(arguments, [value, exponent] if value.type_ == mir::Type::Double && exponent.type_ == mir::Type::Double)
        }
        mir::Intrinsic::StringFromLong | mir::Intrinsic::StringFromLongTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(
                    arguments,
                    [value] if matches!(
                        value.type_,
                        mir::Type::SByte | mir::Type::Short | mir::Type::Int | mir::Type::Long
                    )
                )
        }
        mir::Intrinsic::StringFromULong | mir::Intrinsic::StringFromULongTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(
                    arguments,
                    [value] if matches!(
                        value.type_,
                        mir::Type::Byte | mir::Type::UShort | mir::Type::UInt | mir::Type::ULong
                    )
                )
        }
        mir::Intrinsic::StringFromDouble | mir::Intrinsic::StringFromDoubleTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(arguments, [value] if value.type_ == mir::Type::Double)
        }
        mir::Intrinsic::StringFromFloat | mir::Intrinsic::StringFromFloatTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(arguments, [value] if value.type_ == mir::Type::Float)
        }
        mir::Intrinsic::StringFromBool | mir::Intrinsic::StringFromBoolTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(arguments, [value] if value.type_ == mir::Type::Bool)
        }
        mir::Intrinsic::StringFromChar | mir::Intrinsic::StringFromCharTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(arguments, [value] if value.type_ == mir::Type::Char)
        }
        mir::Intrinsic::StringJoin | mir::Intrinsic::StringJoinTemporary => {
            destination.is_some()
                && return_type == &mir::Type::String
                && !arguments.is_empty()
                && arguments
                    .iter()
                    .all(|argument| argument.type_ == mir::Type::String)
        }
        mir::Intrinsic::ReportRuntimeError(_) | mir::Intrinsic::ListVersionMismatch => {
            destination.is_none() && return_type == &mir::Type::Void && arguments.is_empty()
        }
        mir::Intrinsic::AssertionEqual => {
            destination.is_none()
                && return_type == &mir::Type::Void
                && matches!(arguments, [expected, actual] if expected.type_ == mir::Type::String && actual.type_ == mir::Type::String)
        }
        // `string.TryParse*()`: exactly one `string` receiver, aridade zero,
        // and a destination whose type is *some* concrete enum. The deeper
        // check -- that the enum is actually shaped like `Option<T>` for the
        // matching `T` -- needs the module's enum definitions, which this
        // function does not have; `validate_string_try_parse_targets` (run
        // once over the whole module from `validate_module`) covers that.
        mir::Intrinsic::StringTryParseBool
        | mir::Intrinsic::StringTryParseInt
        | mir::Intrinsic::StringTryParseUInt
        | mir::Intrinsic::StringTryParseLong
        | mir::Intrinsic::StringTryParseULong
        | mir::Intrinsic::StringTryParseFloat
        | mir::Intrinsic::StringTryParseDouble => {
            destination.is_some()
                && matches!(return_type, mir::Type::Enum(_))
                && matches!(arguments, [receiver] if receiver.type_ == mir::Type::String)
        }
        mir::Intrinsic::ConsoleWrite | mir::Intrinsic::ConsoleWriteLine => {
            destination.is_none()
                && *return_type == mir::Type::Void
                && matches!(arguments, [value] if value.type_ == mir::Type::String)
        }
        // `aster.io.ReadLine()`: aridade zero, a destination whose type is
        // *some* concrete enum. `validate_string_try_parse_targets` (reused
        // for this same shape, run once over the whole module) confirms the
        // enum is actually `Option<string>`.
        mir::Intrinsic::ConsoleReadLine | mir::Intrinsic::ConsoleReadLineTemporary => {
            destination.is_some()
                && matches!(return_type, mir::Type::Enum(_))
                && arguments.is_empty()
        }
        // `aster.io.ReadAllText(string)`/`WriteAllText(string, string)`: a
        // destination whose type is *some* concrete enum, with the declared
        // arity and argument types. `validate_file_io_result_shapes` (run
        // once over the whole module) confirms the enum is actually shaped
        // like `Result<T, IOError>`.
        mir::Intrinsic::FileReadAllText(_) | mir::Intrinsic::FileReadAllTextTemporary(_) => {
            destination.is_some()
                && matches!(return_type, mir::Type::Enum(_))
                && matches!(arguments, [path] if path.type_ == mir::Type::String)
        }
        mir::Intrinsic::FileWriteAllText(_) => {
            destination.is_some()
                && matches!(return_type, mir::Type::Enum(_))
                && matches!(arguments, [path, content] if path.type_ == mir::Type::String && content.type_ == mir::Type::String)
        }
        mir::Intrinsic::FileListFiles(_) | mir::Intrinsic::FileListFilesTemporary(_) => {
            destination.is_some()
                && matches!(return_type, mir::Type::Enum(_))
                && matches!(arguments, [directory] if directory.type_ == mir::Type::String)
        }
        mir::Intrinsic::TaskRun => {
            destination.is_some()
                && matches!(
                    (return_type, arguments),
                    (mir::Type::Task(result), [function, values @ ..])
                        if matches!(function.kind, mir::OperandKind::Function(_))
                            && function.type_ == **result
                            && is_worker_transferable(result)
                            && values.iter().all(|value| task_argument_shape(&value.type_))
                            && function_operand_matches(
                                function,
                                &values.iter().map(|value| value.type_.clone()).collect::<Vec<_>>(),
                                result,
                                signatures,
                            )
                )
        }
        mir::Intrinsic::TaskWait => {
            destination.is_some()
                && is_worker_transferable(return_type)
                && matches!(
                    arguments,
                    [argument] if matches!(
                        &argument.type_,
                        mir::Type::Task(inner) if **inner == *return_type
                    )
                )
        }
        mir::Intrinsic::TaskWaitAll => {
            destination.is_some()
                && matches!(
                    (return_type, arguments),
                    (mir::Type::Array(result), [tasks])
                        if is_worker_transferable(result)
                            && tasks.type_ == mir::Type::Array(Box::new(mir::Type::Task(result.clone())))
                )
        }
        mir::Intrinsic::TaskCancel => {
            destination.is_some()
                && *return_type == mir::Type::Bool
                && matches!(arguments, [task] if matches!(task.type_, mir::Type::Task(_)))
        }
        mir::Intrinsic::TaskCancellationRequested => {
            destination.is_some() && *return_type == mir::Type::Bool && arguments.is_empty()
        }
        mir::Intrinsic::AsyncSpawn => {
            destination.is_some()
                && matches!(return_type, mir::Type::Task(_))
                && matches!(
                    arguments,
                    [move_next, count]
                        if matches!(move_next.kind, mir::OperandKind::Function(_))
                            && count.type_ == mir::Type::Int
                            && function_operand_matches(
                                move_next,
                                &[mir::Type::Long],
                                &mir::Type::Int,
                                signatures,
                            )
                )
        }
        mir::Intrinsic::AsyncState => {
            destination.is_some()
                && *return_type == mir::Type::Int
                && matches!(arguments, [handle] if handle.type_ == mir::Type::Long)
        }
        mir::Intrinsic::AsyncSetState => {
            destination.is_none()
                && *return_type == mir::Type::Void
                && matches!(
                    arguments,
                    [handle, new_state]
                        if handle.type_ == mir::Type::Long && new_state.type_ == mir::Type::Int
                )
        }
        mir::Intrinsic::AsyncStoreSlot => {
            destination.is_none()
                && *return_type == mir::Type::Void
                && matches!(
                    arguments,
                    [handle, index, value]
                        if handle.type_ == mir::Type::Long
                            && index.type_ == mir::Type::Int
                            && is_worker_transferable(&value.type_)
                )
        }
        mir::Intrinsic::AsyncLoadSlot => {
            destination.is_some()
                && is_worker_transferable(return_type)
                && matches!(
                    arguments,
                    [handle, index]
                        if handle.type_ == mir::Type::Long && index.type_ == mir::Type::Int
                )
        }
        mir::Intrinsic::AsyncSpawnInner => {
            destination.is_none()
                && *return_type == mir::Type::Void
                && matches!(
                    arguments,
                    [handle, inner, values @ ..]
                        if handle.type_ == mir::Type::Long
                            && matches!(inner.kind, mir::OperandKind::Function(_))
                            && values.iter().all(|value| task_argument_shape(&value.type_))
                            && function_operand_matches(
                                inner,
                                &values.iter().map(|value| value.type_.clone()).collect::<Vec<_>>(),
                                &inner.type_,
                                signatures,
                            )
                )
        }
        mir::Intrinsic::AsyncAwaitResult => {
            destination.is_some()
                && is_worker_transferable(return_type)
                && matches!(arguments, [handle] if handle.type_ == mir::Type::Long)
        }
        mir::Intrinsic::AsyncSetResult => {
            destination.is_none()
                && *return_type == mir::Type::Void
                && matches!(
                    arguments,
                    [handle, value]
                        if handle.type_ == mir::Type::Long && is_worker_transferable(&value.type_)
                )
        }
        mir::Intrinsic::ParallelFor => {
            destination.is_none()
                && *return_type == mir::Type::Void
                && matches!(
                    arguments,
                    [start, end, body]
                        if start.type_ == mir::Type::Int
                            && end.type_ == mir::Type::Int
                            && matches!(body.kind, mir::OperandKind::Function(_))
                            && function_operand_matches(
                                body,
                                &[mir::Type::Int],
                                &mir::Type::Void,
                                signatures,
                            )
                )
        }
        mir::Intrinsic::ParallelForEach => {
            destination.is_none()
                && *return_type == mir::Type::Void
                && matches!(
                    arguments,
                    [values, body]
                        if matches!(
                            &values.type_,
                            mir::Type::Array(element) if **element == body.type_
                        )
                            && matches!(body.kind, mir::OperandKind::Function(_))
                            && is_worker_transferable(&body.type_)
                            && function_operand_matches(
                                body,
                                std::slice::from_ref(&body.type_),
                                &mir::Type::Void,
                                signatures,
                            )
                )
        }
        mir::Intrinsic::ParallelReduce => {
            destination.is_some()
                && is_worker_transferable(return_type)
                && matches!(
                    arguments,
                    [values, identity, accumulate, combine]
                        if matches!(
                            &values.type_,
                            mir::Type::Array(element) if **element == accumulate.type_
                        )
                            && is_worker_transferable(&accumulate.type_)
                            && identity.type_ == *return_type
                            && matches!(accumulate.kind, mir::OperandKind::Function(_))
                            && function_operand_matches(
                                accumulate,
                                &[return_type.clone(), accumulate.type_.clone()],
                                return_type,
                                signatures,
                            )
                            && combine.type_ == *return_type
                            && matches!(combine.kind, mir::OperandKind::Function(_))
                            && function_operand_matches(
                                combine,
                                &[return_type.clone(), return_type.clone()],
                                return_type,
                                signatures,
                            )
                )
        }
    };
    if valid {
        Ok(())
    } else {
        let arguments = arguments
            .iter()
            .map(|argument| format!("{:?}", argument.type_))
            .collect::<Vec<_>>()
            .join(", ");
        Err(BackendError::new(format!(
            "function `{function_name}` contains a malformed {intrinsic:?} runtime intrinsic: found ({arguments}) -> {return_type:?}"
        )))
    }
}

fn is_math_unary_operation(operand: &mir::Operand) -> bool {
    operand.type_ == mir::Type::Int
        && matches!(
            &operand.kind,
            mir::OperandKind::Constant(mir::Constant::Integer(code))
                if matches!(code.as_str(), "0" | "1" | "2" | "3" | "4" | "5" | "6")
        )
}

fn function_operand_matches(
    operand: &mir::Operand,
    parameters: &[mir::Type],
    return_type: &mir::Type,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
) -> bool {
    let mir::OperandKind::Function(symbol) = operand.kind else {
        return false;
    };
    signatures.get(&symbol).is_some_and(|function| {
        function.return_type == *return_type
            && function.parameters.len() == parameters.len()
            && function
                .parameters
                .iter()
                .zip(parameters)
                .all(|(actual, expected)| actual.type_ == *expected)
    })
}

#[allow(clippy::too_many_lines)]
fn validate_rvalue(
    value: &mir::Rvalue,
    function_name: &str,
    implementations: &HashSet<(mir::SymbolId, mir::SymbolId)>,
    locals: &HashMap<mir::LocalId, mir::Type>,
) -> Result<(), BackendError> {
    validate_value_type(&value.type_, function_name)?;
    if matches!(value.type_, mir::Type::Float | mir::Type::Double)
        && matches!(
            value.kind,
            mir::RvalueKind::Binary {
                operator: mir::BinaryOperator::Remainder,
                ..
            }
        )
    {
        return Err(BackendError::new(format!(
            "floating-point remainder is not yet supported by the JIT in function `{function_name}`"
        )));
    }
    match &value.kind {
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            for field in fields {
                validate_operand_with_proven_bounds(&field.value, function_name, true)?;
            }
            Ok(())
        }
        mir::RvalueKind::ArrayLength(array) => {
            validate_operand_with_proven_bounds(array, function_name, true)
        }
        mir::RvalueKind::ListLength(list) => {
            validate_operand_with_proven_bounds(list, function_name, true)?;
            if !matches!(list.type_, mir::Type::List(_)) {
                return Err(BackendError::new(format!(
                    "function `{function_name}` has a `ListLength` reading a non-`List<T>` receiver"
                )));
            }
            if value.type_ != mir::Type::Int {
                return Err(BackendError::new(format!(
                    "function `{function_name}` has a `ListLength` whose result type is not `int`"
                )));
            }
            Ok(())
        }
        mir::RvalueKind::DictionaryLength(dictionary) => {
            validate_operand_with_proven_bounds(dictionary, function_name, true)?;
            validate_dictionary_operand_locals(
                dictionary,
                "DictionaryLength",
                function_name,
                locals,
            )?;
            if !matches!(dictionary.type_, mir::Type::Dictionary(_, _)) {
                return Err(BackendError::new(format!(
                    "function `{function_name}` has a `DictionaryLength` reading a non-`Dictionary<K, V>` receiver"
                )));
            }
            if value.type_ != mir::Type::Int {
                return Err(BackendError::new(format!(
                    "function `{function_name}` has a `DictionaryLength` whose result type is not `int`"
                )));
            }
            Ok(())
        }
        mir::RvalueKind::ListVersion(list) => {
            validate_operand_with_proven_bounds(list, function_name, true)?;
            if !matches!(list.type_, mir::Type::List(_)) {
                return Err(BackendError::new(format!(
                    "function `{function_name}` has a `ListVersion` reading a non-`List<T>` receiver"
                )));
            }
            if value.type_ != mir::Type::Long {
                return Err(BackendError::new(format!(
                    "function `{function_name}` has a `ListVersion` whose result type is not `long`"
                )));
            }
            Ok(())
        }
        mir::RvalueKind::StringByteLength(operand) => {
            validate_operand_with_proven_bounds(operand, function_name, true)?;
            if operand.type_ != mir::Type::String {
                return Err(BackendError::new(format!(
                    "function `{function_name}` has a `StringByteLength` reading a non-`string` receiver"
                )));
            }
            if value.type_ != mir::Type::Int {
                return Err(BackendError::new(format!(
                    "function `{function_name}` has a `StringByteLength` whose result type is not `int`"
                )));
            }
            Ok(())
        }
        mir::RvalueKind::MakeInterface {
            object,
            class,
            interface,
        } => {
            if !matches!(object.type_, mir::Type::Class(_)) {
                return Err(BackendError::new(format!(
                    "function `{function_name}` converts a non-class value to an interface"
                )));
            }
            if object.type_ != mir::Type::Class(*class)
                || value.type_ != mir::Type::Interface(*interface)
                || !implementations.contains(&(*class, *interface))
            {
                return Err(BackendError::new(format!(
                    "function `{function_name}` contains an invalid class-to-interface conversion"
                )));
            }
            validate_operand_with_proven_bounds(object, function_name, true)
        }
        mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => {
            validate_operand_with_proven_bounds(operand, function_name, true)
        }
        mir::RvalueKind::Binary {
            left,
            operator,
            right,
        } => {
            validate_operand_with_proven_bounds(left, function_name, true)?;
            validate_operand_with_proven_bounds(right, function_name, true)?;
            if matches!(
                operator,
                mir::BinaryOperator::Divide | mir::BinaryOperator::Remainder
            ) {
                validate_direct_operand_local(left, "binary operation", function_name, locals)?;
                validate_direct_operand_local(right, "binary operation", function_name, locals)?;
            }
            validate_division_or_remainder_shape(value, *operator, left, right, function_name)
        }
        mir::RvalueKind::Equality { left, right, .. } => {
            validate_operand_with_proven_bounds(left, function_name, true)?;
            validate_operand_with_proven_bounds(right, function_name, true)
        }
    }
}

fn validate_division_or_remainder_shape(
    value: &mir::Rvalue,
    operator: mir::BinaryOperator,
    left: &mir::Operand,
    right: &mir::Operand,
    function_name: &str,
) -> Result<(), BackendError> {
    if !matches!(
        operator,
        mir::BinaryOperator::Divide | mir::BinaryOperator::Remainder
    ) {
        return Ok(());
    }
    if left.type_ != right.type_ || value.type_ != left.type_ {
        return Err(BackendError::new(format!(
            "function `{function_name}` has division or remainder operands and result with mismatched types"
        )));
    }
    let integer = primitive(&left.type_).is_some_and(aster_types::Primitive::is_integer);
    let supported = integer
        || (operator == mir::BinaryOperator::Divide
            && matches!(left.type_, mir::Type::Float | mir::Type::Double));
    if supported {
        Ok(())
    } else {
        Err(BackendError::new(format!(
            "function `{function_name}` has division or remainder with an unsupported operand type"
        )))
    }
}

fn validate_terminator(
    terminator: &mir::Terminator,
    function_name: &str,
    blocks: &HashSet<mir::BasicBlockId>,
) -> Result<(), BackendError> {
    match terminator {
        mir::Terminator::Goto(target) => validate_block_target(*target, function_name, blocks),
        mir::Terminator::Branch {
            condition,
            then_block,
            else_block,
        } => {
            validate_operand(condition, function_name)?;
            if condition.type_ != mir::Type::Bool {
                return Err(BackendError::new(format!(
                    "function `{function_name}` has a `Branch` whose condition is not `bool`"
                )));
            }
            validate_block_target(*then_block, function_name, blocks)?;
            validate_block_target(*else_block, function_name, blocks)
        }
        mir::Terminator::Return(Some(value)) => validate_operand(value, function_name),
        mir::Terminator::Return(None) | mir::Terminator::End | mir::Terminator::Unreachable => {
            Ok(())
        }
    }
}

fn validate_block_target(
    target: mir::BasicBlockId,
    function_name: &str,
    blocks: &HashSet<mir::BasicBlockId>,
) -> Result<(), BackendError> {
    if blocks.contains(&target) {
        Ok(())
    } else {
        Err(BackendError::new(format!(
            "function `{function_name}` references unknown basic block {target:?}"
        )))
    }
}

fn validate_operand(operand: &mir::Operand, function_name: &str) -> Result<(), BackendError> {
    validate_operand_with_proven_bounds(operand, function_name, false)
}

fn validate_operand_with_proven_bounds(
    operand: &mir::Operand,
    function_name: &str,
    allow_proven: bool,
) -> Result<(), BackendError> {
    validate_value_type(&operand.type_, function_name)?;
    match &operand.kind {
        mir::OperandKind::Constant(mir::Constant::Integer(value)) => integer_constant_bits(
            value,
            &operand.type_,
        )
        .map(|_| ())
        .map_err(|_| {
            BackendError::new(format!(
                "integer constant `{value}` in function `{function_name}` does not fit `{}`",
                type_name(&operand.type_)
            ))
        }),
        mir::OperandKind::Constant(_) | mir::OperandKind::Copy(mir::Place::Local(_)) => Ok(()),
        mir::OperandKind::Copy(place @ mir::Place::Index { element_type, .. }) => {
            validate_place_with_proven_bounds(place, function_name, allow_proven)?;
            if operand.type_ != *element_type {
                return Err(BackendError::new(format!(
                    "function `{function_name}` has an array-index read whose result type does not match the indexed place's element type"
                )));
            }
            Ok(())
        }
        mir::OperandKind::Copy(place) => {
            validate_place_with_proven_bounds(place, function_name, allow_proven)
        }
        mir::OperandKind::Function(_) => Err(unsupported(function_name, "function values")),
    }
}

fn validate_place(place: &mir::Place, function_name: &str) -> Result<(), BackendError> {
    validate_place_with_proven_bounds(place, function_name, false)
}

fn validate_place_with_proven_bounds(
    place: &mir::Place,
    function_name: &str,
    allow_proven: bool,
) -> Result<(), BackendError> {
    match place {
        mir::Place::Local(_) => Ok(()),
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            validate_place_with_proven_bounds(base, function_name, allow_proven)
        }
        mir::Place::Index {
            array,
            index,
            element_type,
            bounds,
        } => {
            if matches!(bounds, mir::ArrayBounds::Proven { .. }) && !allow_proven {
                return Err(BackendError::new(format!(
                    "function `{function_name}` uses proven array bounds outside an assignment"
                )));
            }
            validate_operand_with_proven_bounds(array, function_name, allow_proven)?;
            validate_operand_with_proven_bounds(index, function_name, allow_proven)?;
            validate_value_type(element_type, function_name)
        }
        mir::Place::ObjectField { object, .. } => {
            validate_operand_with_proven_bounds(object, function_name, allow_proven)
        }
        mir::Place::Symbol(_) => Err(unsupported(
            function_name,
            "module globals, classes, and objects",
        )),
    }
}

/// Whether a value of `type_` can cross a worker boundary (`Task.Run`,
/// `Parallel.For`/`ForEach`, or an async frame slot): copied entirely by
/// value, with a fixed-width ABI and no arena identity. Derives from
/// `aster-types`'s single `Primitive::is_worker_transferable` fact by way of
/// `values::primitive`, the crate's one MIR-type-to-primitive adapter,
/// instead of a second hand-written list that could drift from it (see
/// `aster_compiler::semantic::general::calls::transferable`, the equivalent
/// predicate at the frontend, which derives from the same
/// `aster-types` fact independently).
fn is_worker_transferable(type_: &mir::Type) -> bool {
    primitive(type_).is_some_and(aster_types::Primitive::is_worker_transferable)
}

fn task_argument_shape(type_: &mir::Type) -> bool {
    is_worker_transferable(type_) || matches!(type_, mir::Type::User(_) | mir::Type::Enum(_))
}

fn validate_task_argument_transfer(
    module: &mir::Module,
    structs: &HashMap<mir::SymbolId, &mir::StructDefinition>,
    enums: &HashMap<mir::SymbolId, &mir::EnumDefinition>,
) -> Result<(), BackendError> {
    fn transferable(
        type_: &mir::Type,
        structs: &HashMap<mir::SymbolId, &mir::StructDefinition>,
        enums: &HashMap<mir::SymbolId, &mir::EnumDefinition>,
        visiting: &mut HashSet<mir::SymbolId>,
    ) -> bool {
        if is_worker_transferable(type_) {
            return true;
        }
        let symbol = match type_ {
            mir::Type::User(symbol) | mir::Type::Enum(symbol) => *symbol,
            _ => return false,
        };
        if !visiting.insert(symbol) {
            return false;
        }
        let result = match type_ {
            mir::Type::User(_) => structs.get(&symbol).is_some_and(|definition| {
                definition
                    .fields
                    .iter()
                    .all(|field| transferable(&field.type_, structs, enums, visiting))
            }),
            mir::Type::Enum(_) => enums.get(&symbol).is_some_and(|definition| {
                definition.cases.iter().all(|case| {
                    case.fields
                        .iter()
                        .all(|field| transferable(&field.type_, structs, enums, visiting))
                })
            }),
            _ => false,
        };
        visiting.remove(&symbol);
        result
    }

    for function in &module.functions {
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            let mir::Instruction::CallIntrinsic {
                intrinsic,
                arguments,
                ..
            } = instruction
            else {
                continue;
            };
            let skip = match intrinsic {
                mir::Intrinsic::TaskRun => 1,
                mir::Intrinsic::AsyncSpawnInner => 2,
                _ => continue,
            };
            for argument in arguments.iter().skip(skip) {
                if !transferable(&argument.type_, structs, enums, &mut HashSet::new()) {
                    return Err(BackendError::new(format!(
                        "function `{}` contains a malformed TaskRun with non-transferable argument type `{}`",
                        function.name,
                        type_name(&argument.type_)
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Whether the JIT backend can execute code that produces or consumes
/// `type_` today. Wider than [`is_worker_transferable`]: `string` executes
/// sequentially but cannot cross a worker boundary (arena identity, no fixed
/// width). Derives from `Primitive::is_backend_executable`.
fn executable_value_type(type_: &mir::Type) -> bool {
    primitive(type_).is_some_and(aster_types::Primitive::is_backend_executable)
}

fn validate_value_type(type_: &mir::Type, function_name: &str) -> Result<(), BackendError> {
    if let mir::Type::Array(element) = type_ {
        if matches!(**element, mir::Type::Array(_)) {
            return Err(unsupported(function_name, "nested arrays"));
        }
        return validate_value_type(element, function_name);
    }
    if let mir::Type::List(element) = type_ {
        // `List<T>` itself is always a plain reference (pointer-width, see
        // `layouts::layout_of`); only `T` needs the same value-type rule
        // (rejects `void`/`decimal`) that every other container element
        // already gets. Nominal existence for `T` is checked specifically
        // where a list is actually allocated (`validate_list_element_type`).
        return validate_value_type(element, function_name);
    }
    if let mir::Type::Dictionary(key, value) = type_ {
        validate_dictionary_key_type(key, function_name)?;
        return validate_value_type(value, function_name);
    }
    if executable_value_type(type_)
        || matches!(
            type_,
            mir::Type::User(_)
                | mir::Type::Class(_)
                | mir::Type::Interface(_)
                | mir::Type::Enum(_)
                | mir::Type::Task(_)
        )
    {
        Ok(())
    } else if *type_ == mir::Type::Decimal {
        Err(BackendError::new(format!(
            "`decimal` is checked by the compiler but cannot execute yet in function `{function_name}`; a dedicated decimal runtime representation is the planned next step"
        )))
    } else {
        Err(unsupported(
            function_name,
            &format!("values of type `{}`", type_name(type_)),
        ))
    }
}

fn validate_return_type(type_: &mir::Type, function_name: &str) -> Result<(), BackendError> {
    if *type_ == mir::Type::Void {
        Ok(())
    } else {
        validate_value_type(type_, function_name)
    }
}

fn unsupported(function_name: &str, feature: &str) -> BackendError {
    BackendError::new(format!(
        "Cranelift JIT does not yet support {feature} in function `{function_name}`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn division_shape_rejects_reference_and_aggregate_operands() {
        for type_ in [
            mir::Type::String,
            mir::Type::Array(Box::new(mir::Type::Int)),
            mir::Type::User(mir::SymbolId(1)),
        ] {
            let left = mir::Operand {
                type_: type_.clone(),
                kind: mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(0))),
            };
            let right = mir::Operand {
                type_: type_.clone(),
                kind: mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(1))),
            };
            let value = mir::Rvalue {
                type_,
                kind: mir::RvalueKind::Binary {
                    left: left.clone(),
                    operator: mir::BinaryOperator::Divide,
                    right: right.clone(),
                },
            };

            let error = validate_division_or_remainder_shape(
                &value,
                mir::BinaryOperator::Divide,
                &left,
                &right,
                "Malformed",
            )
            .expect_err("reference or aggregate division must be rejected");
            assert!(error.message().contains("unsupported operand type"));
        }
    }

    const OWNED_REGION_SOURCE: &str = r"
        internal int[] Make(int value) { return [value]; }
        public int Main() {
            int total = 0;
            for (int i = 0; i < 10; i++) {
                int[] value = Make(i);
                total += value[0];
            }
            return total;
        }
    ";

    fn owned_region_module() -> mir::Module {
        aster_compiler::compile(OWNED_REGION_SOURCE)
            .expect("owned-region validation source compiles")
            .mir
    }
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_STRING_BUILDER_ID: AtomicU64 = AtomicU64::new(0);

    fn fine_builder_allocation(local: u32, region: mir::AllocationRegion) -> mir::Instruction {
        mir::Instruction::AllocateStringBuilder {
            destination: mir::Place::Local(mir::LocalId(local)),
            class: mir::SymbolId(900),
            region,
        }
    }

    fn fine_builder_append(local: u32) -> mir::Instruction {
        mir::Instruction::StringBuilderAppend {
            builder: mir::Operand {
                type_: mir::Type::Class(mir::SymbolId(900)),
                kind: mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(local))),
            },
            value: mir::Operand {
                type_: mir::Type::String,
                kind: mir::OperandKind::Constant(mir::Constant::String("x".to_owned())),
            },
            class: mir::SymbolId(900),
        }
    }

    #[test]
    fn fine_builder_provenance_state_is_exact_at_joins_and_resets_per_region() {
        let id = mir::TemporarySubregionId(7);
        let allocation = fine_builder_allocation(1, mir::AllocationRegion::Temporary);
        let append = fine_builder_append(1);
        let mut state = FineExecutionState::inactive();

        state.enter(id).expect("fine Enter starts ownership domain");
        assert!(state.owned.is_empty());
        state
            .validate_builder_instruction(&allocation)
            .expect("Temporary direct-local builder becomes owned");
        assert_eq!(
            state.owned,
            BTreeMap::from([(1, FineOwnedKind::StringBuilder)])
        );
        state
            .validate_builder_instruction(&append)
            .expect("Append on the owned local is structurally valid");

        let identical_join = state.clone();
        assert_eq!(state, identical_join, "identical incoming sets join");
        let mut mismatched_join = state.clone();
        mismatched_join
            .owned
            .insert(2, FineOwnedKind::StringBuilder);
        assert_ne!(
            state, mismatched_join,
            "different incoming sets cannot join"
        );

        state
            .exit(id)
            .expect("matching Exit closes ownership domain");
        assert_eq!(state, FineExecutionState::inactive());
        state
            .enter(id)
            .expect("next loop iteration starts a new domain");
        assert!(state.owned.is_empty());
        assert!(state.validate_builder_instruction(&append).is_err());
        assert!(state.enter(mir::TemporarySubregionId(8)).is_err());
        assert!(state.exit(mir::TemporarySubregionId(8)).is_err());

        let persistent = fine_builder_allocation(1, mir::AllocationRegion::Persistent);
        let mut persistent_state = FineExecutionState::inactive();
        persistent_state.enter(id).expect("fine Enter");
        persistent_state
            .validate_builder_instruction(&persistent)
            .expect("Persistent builder is observed but not owned");
        assert!(persistent_state.owned.is_empty());
        assert!(
            persistent_state
                .validate_builder_instruction(&append)
                .is_err()
        );

        assert!(temporary_subregion_instruction_is_executable(&allocation));
        assert!(temporary_subregion_instruction_is_executable(&append));
    }

    fn string_builder_module() -> mir::Module {
        let id = NEXT_STRING_BUILDER_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aster-invalid-string-builder-{}-{id}.aster",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "using aster.core; public string Main() { StringBuilder builder = new StringBuilder(); builder.Append(\"x\"); return builder.ToString(); }",
        )
        .expect("write StringBuilder validation source");
        let result = aster_compiler::compile_project(&path)
            .expect("StringBuilder validation source compiles")
            .compilation
            .mir;
        std::fs::remove_file(path).expect("remove StringBuilder validation source");
        result
    }

    #[test]
    fn malformed_string_builder_operations_are_rejected_before_codegen() {
        let valid = string_builder_module();
        validate_module(&valid).expect("compiler-produced builder MIR is valid");

        let mut unknown_class = valid.clone();
        let class = unknown_class
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::AllocateStringBuilder { class, .. } => Some(class),
                _ => None,
            })
            .expect("builder allocation instruction");
        *class = mir::SymbolId(u32::MAX);
        assert!(
            validate_module(&unknown_class)
                .expect_err("unknown builder class must fail")
                .message()
                .contains("unknown class identity")
        );

        let mut wrong_append = valid.clone();
        let append = wrong_append
            .functions
            .iter_mut()
            .find(|function| function.name == "Main")
            .into_iter()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::StringBuilderAppend { value, .. } => Some(value),
                _ => None,
            })
            .expect("append instruction");
        append.type_ = mir::Type::Int;
        assert!(
            validate_module(&wrong_append)
                .expect_err("non-string append must fail")
                .message()
                .contains("StringBuilder.Append")
        );

        let mut constant_receiver = valid.clone();
        let builder = constant_receiver
            .functions
            .iter_mut()
            .find(|function| function.name == "Main")
            .into_iter()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::StringBuilderAppend { builder, .. } => Some(builder),
                _ => None,
            })
            .expect("append receiver");
        builder.kind = mir::OperandKind::Constant(mir::Constant::String("invalid".to_owned()));
        assert!(
            validate_module(&constant_receiver)
                .expect_err("constant builder receiver must fail")
                .message()
                .contains("StringBuilder.Append")
        );

        let mut wrong_result = valid;
        let function = wrong_result
            .functions
            .iter_mut()
            .find(|function| function.name == "Main")
            .expect("Main exists");
        let builder_local = function
            .locals
            .iter()
            .find(|local| matches!(local.type_, mir::Type::Class(_)))
            .expect("builder local exists")
            .id;
        let destination = function
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::StringBuilderToString { destination, .. } => Some(destination),
                _ => None,
            })
            .expect("ToString instruction");
        *destination = mir::Place::Local(builder_local);
        assert!(
            validate_module(&wrong_result)
                .expect_err("non-string destination must fail")
                .message()
                .contains("non-string destination")
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn malformed_standard_library_intrinsics_are_rejected_before_codegen() {
        let id = NEXT_STRING_BUILDER_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aster-invalid-stdlib-usability-{}-{id}.aster",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"
                using aster.collections;
                using aster.math;
                using aster.text;
                public double Root() { return Math.Sqrt(81d); }
                public int Text() { string[] parts = String.Split("a,b", ","); return parts.Length; }
                public int ListOps() {
                    List<int> items = new List<int>();
                    items.Add(1);
                    items.Set(0, 2);
                    int[] snapshot = items.ToArray();
                    items.Clear();
                    return snapshot.Length;
                }
                public int Main() {
                    Dictionary<string, int> values = new Dictionary<string, int>();
                    values.Add("a", 1);
                    string[] keys = values.Keys();
                    return keys.Length;
                }
            "#,
        )
        .expect("write standard-library validation source");
        let valid = aster_compiler::compile_project(&path)
            .expect("standard-library validation source compiles")
            .compilation
            .mir;
        std::fs::remove_file(path).expect("remove standard-library validation source");
        validate_module(&valid).expect("compiler-produced standard-library MIR is valid");

        let mut invalid_math = valid.clone();
        let operation = invalid_math
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::MathUnaryDouble,
                    arguments,
                    ..
                } => arguments.get_mut(1),
                _ => None,
            })
            .expect("Math.Sqrt intrinsic");
        operation.kind = mir::OperandKind::Constant(mir::Constant::Integer("99".to_owned()));
        let error =
            validate_module(&invalid_math).expect_err("unknown math unary operation must fail");
        assert!(
            error.message().contains("malformed MathUnaryDouble"),
            "unexpected math validation diagnostic: {}",
            error.message()
        );

        let mut invalid_math_destination = valid.clone();
        let math_function_index = invalid_math_destination
            .functions
            .iter()
            .position(|function| {
                function.blocks.iter().any(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(
                            instruction,
                            mir::Instruction::CallIntrinsic {
                                intrinsic: mir::Intrinsic::MathUnaryDouble,
                                ..
                            }
                        )
                    })
                })
            })
            .expect("math intrinsic wrapper function");
        let destination_local = invalid_math_destination.functions[math_function_index]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::CallIntrinsic {
                    destination: Some(mir::Place::Local(local)),
                    intrinsic: mir::Intrinsic::MathUnaryDouble,
                    ..
                } => Some(*local),
                _ => None,
            })
            .expect("Math.Sqrt destination");
        invalid_math_destination.functions[math_function_index]
            .locals
            .iter_mut()
            .find(|local| local.id == destination_local)
            .expect("Math.Sqrt destination local")
            .type_ = mir::Type::Int;
        let error = validate_module(&invalid_math_destination)
            .expect_err("wrong math destination type must fail");
        assert!(
            error.message().contains("stores MathUnaryDouble result"),
            "unexpected math destination diagnostic: {}",
            error.message()
        );

        let mut invalid_string = valid.clone();
        let return_type = invalid_string
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::StringSplit | mir::Intrinsic::StringSplitTemporary,
                    return_type,
                    ..
                } => Some(return_type),
                _ => None,
            })
            .expect("String.Split intrinsic");
        *return_type = mir::Type::String;
        assert!(
            validate_module(&invalid_string)
                .expect_err("wrong String.Split return type must fail")
                .message()
                .contains("stores StringSplit result")
        );

        let mut invalid_list_index = valid.clone();
        let index = invalid_list_index
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::ListSet { index, .. } => Some(index),
                _ => None,
            })
            .expect("List.Set instruction");
        index.type_ = mir::Type::Long;
        assert!(
            validate_module(&invalid_list_index)
                .expect_err("wrong List.Set index type must fail")
                .message()
                .contains("ListSet")
        );

        let mut invalid_list_snapshot = valid.clone();
        let element_type = invalid_list_snapshot
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::ListToArray { element_type, .. } => Some(element_type),
                _ => None,
            })
            .expect("List.ToArray instruction");
        *element_type = mir::Type::String;
        assert!(
            validate_module(&invalid_list_snapshot)
                .expect_err("wrong List.ToArray element metadata must fail")
                .message()
                .contains("ListToArray")
        );

        let mut invalid_list_receiver = valid.clone();
        let list = invalid_list_receiver
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::ListClear { list } => Some(list),
                _ => None,
            })
            .expect("List.Clear instruction");
        list.type_ = mir::Type::List(Box::new(mir::Type::String));
        assert!(
            validate_module(&invalid_list_receiver)
                .expect_err("List.Clear local/type disagreement must fail")
                .message()
                .contains("operand type does not match its local")
        );

        let mut invalid_snapshot = valid;
        let key_type = invalid_snapshot
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::DictionaryKeys { key_type, .. } => Some(key_type),
                _ => None,
            })
            .expect("Dictionary.Keys instruction");
        *key_type = mir::Type::Int;
        assert!(
            validate_module(&invalid_snapshot)
                .expect_err("wrong snapshot component type must fail")
                .message()
                .contains("snapshot metadata")
        );
    }

    #[test]
    fn duplicate_function_symbols_are_rejected_before_codegen() {
        let mut module = aster_compiler::compile(
            "public int First() { return 1; } \
             public int Second() { return 2; } \
             public int Main() { return First(); }",
        )
        .expect("source compiles")
        .mir;
        let duplicate = module
            .functions
            .iter()
            .find(|function| function.name == "First")
            .expect("First exists")
            .symbol;
        module
            .functions
            .iter_mut()
            .find(|function| function.name == "Second")
            .expect("Second exists")
            .symbol = duplicate;

        let error = validate_module(&module).expect_err("duplicate symbols must be rejected");
        assert!(error.message().contains("duplicate function symbol"));
    }

    #[test]
    fn missing_entry_block_is_rejected_before_codegen() {
        let mut module = aster_compiler::compile("public int Main() { return 1; }")
            .expect("source compiles")
            .mir;
        module
            .functions
            .iter_mut()
            .find(|function| function.name == "Main")
            .expect("Main exists")
            .entry = mir::BasicBlockId(u32::MAX);

        let error = validate_module(&module).expect_err("missing entry block must be rejected");
        assert!(error.message().contains("unknown entry block"));
    }

    #[test]
    fn duplicate_basic_block_ids_are_rejected_before_codegen() {
        let mut module = aster_compiler::compile(
            "public int Main() { int value = 0; if (value == 0) { value = 1; } return value; }",
        )
        .expect("source compiles")
        .mir;
        let function = module
            .functions
            .iter_mut()
            .find(|function| function.name == "Main")
            .expect("Main exists");
        assert!(
            function.blocks.len() > 1,
            "if lowering creates multiple blocks"
        );
        function.blocks[1].id = function.blocks[0].id;

        let error = validate_module(&module).expect_err("duplicate blocks must be rejected");
        assert!(error.message().contains("duplicate basic block"));
    }

    #[test]
    fn unknown_terminator_target_is_rejected_before_codegen() {
        let mut module = aster_compiler::compile("public int Main() { return 1; }")
            .expect("source compiles")
            .mir;
        let function = module
            .functions
            .iter_mut()
            .find(|function| function.name == "Main")
            .expect("Main exists");
        function.blocks[0].terminator = mir::Terminator::Goto(mir::BasicBlockId(u32::MAX));

        let error = validate_module(&module).expect_err("unknown target must be rejected");
        assert!(error.message().contains("unknown basic block"));
    }

    #[test]
    fn malformed_string_operation_signature_is_rejected_before_codegen() {
        let mut module =
            aster_compiler::compile("public bool Run() { return \"aster\".Contains(\"a\"); }")
                .expect("source compiles")
                .mir;
        let call = module.functions[0]
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::StringContains,
                        ..
                    }
                )
            })
            .expect("contains intrinsic exists");
        let mir::Instruction::CallIntrinsic { arguments, .. } = call else {
            unreachable!();
        };
        arguments[1] = mir::Operand {
            type_: mir::Type::Int,
            kind: mir::OperandKind::Constant(mir::Constant::Integer("1".to_owned())),
        };

        let error = validate_module(&module).expect_err("wrong argument type must be rejected");
        assert!(error.message().contains("StringContains"));
        assert!(error.message().contains("String, Int"));
    }

    #[test]
    fn string_operation_rejects_undeclared_operands_before_codegen() {
        let mut module =
            aster_compiler::compile("public int Run(string text) { return text.IndexOf(\"a\"); }")
                .expect("source compiles")
                .mir;
        let call = module.functions[0]
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::StringIndexOf,
                        ..
                    }
                )
            })
            .expect("index intrinsic exists");
        let mir::Instruction::CallIntrinsic { arguments, .. } = call else {
            unreachable!();
        };
        arguments[0].kind = mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(u32::MAX)));

        let error = validate_module(&module).expect_err("unknown operand must be rejected");
        assert!(error.message().contains("undeclared local"));
        assert!(error.message().contains("StringIndexOf"));
    }

    #[test]
    fn string_operation_rejects_an_incompatible_destination_before_codegen() {
        let mut module =
            aster_compiler::compile("public string Run(string text) { return text.Substring(1); }")
                .expect("source compiles")
                .mir;
        let call = module.functions[0]
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::StringSubstringFrom
                            | mir::Intrinsic::StringSubstringFromTemporary,
                        ..
                    }
                )
            })
            .expect("substring intrinsic exists");
        let mir::Instruction::CallIntrinsic { return_type, .. } = call else {
            unreachable!();
        };
        *return_type = mir::Type::Int;

        let error = validate_module(&module).expect_err("wrong destination type must be rejected");
        assert!(error.message().contains("StringSubstringFrom"));
        assert!(error.message().contains("destination `String`"));
    }

    #[test]
    fn parallel_for_each_rejects_element_metadata_that_disagrees_with_the_body_signature() {
        let mut module = aster_compiler::compile(
            "public void Body(int value) { } \
             public int Main() { int[] values = [1]; Parallel.ForEach(values, Body); return 0; }",
        )
        .expect("source compiles")
        .mir;
        for function in &mut module.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    if let mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::ParallelForEach,
                        arguments,
                        ..
                    } = instruction
                    {
                        arguments[1].type_ = mir::Type::Double;
                    }
                }
            }
        }

        let error = validate_module(&module).expect_err("mismatched scalar width must be rejected");
        assert!(error.message().contains("malformed ParallelForEach"));
    }

    #[test]
    fn task_run_rejects_a_symbol_with_an_incompatible_signature() {
        let mut module = aster_compiler::compile(
            "public int Compute() { return 1; } \
             public void Wrong(int value) { } \
             public int Main() { return Task.Run(Compute).Wait(); }",
        )
        .expect("source compiles")
        .mir;
        let wrong = module
            .functions
            .iter()
            .find(|function| function.name == "Wrong")
            .expect("Wrong is declared")
            .symbol;
        for function in &mut module.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    if let mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskRun,
                        arguments,
                        ..
                    } = instruction
                    {
                        arguments[0].kind = mir::OperandKind::Function(wrong);
                    }
                }
            }
        }

        let error = validate_module(&module).expect_err("wrong task ABI must be rejected");
        assert!(error.message().contains("malformed TaskRun"));
    }

    /// Adulterated MIR (never producible by the compiler itself, since the
    /// semantic `transferable` gate already rejects `Task<Box>` at the
    /// source level) must still be caught here, not reach
    /// `execution::invoke_finalized`'s `unreachable!()` fallback on a worker
    /// thread. This is the MIR-level half of the worker-boundary guarantee:
    /// the frontend check alone is not the only line of defense.
    #[test]
    fn task_run_rejects_a_non_transferable_result_type() {
        let mut module = aster_compiler::compile(
            "public class Box { public Box() {} } \
             public Box Make() { return new Box(); } \
             public int Compute() { return 1; } \
             public int Main() { return Task.Run(Compute).Wait(); }",
        )
        .expect("source compiles")
        .mir;
        let make = module
            .functions
            .iter()
            .find(|function| function.name == "Make")
            .expect("Make is declared")
            .symbol;
        let box_class = module
            .classes
            .iter()
            .find(|class| class.name == "Box")
            .expect("Box is declared")
            .symbol;
        for function in &mut module.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    if let mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskRun,
                        arguments,
                        return_type,
                        ..
                    } = instruction
                    {
                        arguments[0].kind = mir::OperandKind::Function(make);
                        arguments[0].type_ = mir::Type::Class(box_class);
                        *return_type = mir::Type::Task(Box::new(mir::Type::Class(box_class)));
                    }
                }
            }
        }

        let error = validate_module(&module)
            .expect_err("a Task.Run result with arena identity must be rejected");
        assert!(error.message().contains("malformed TaskRun"));
    }

    #[test]
    fn task_run_rejects_an_argument_that_disagrees_with_the_target_signature() {
        let mut module = aster_compiler::compile(
            "public int Add(int value) { return value + 1; } \
             public int Main() { return Task.Run(Add, 41).Wait(); }",
        )
        .expect("source compiles")
        .mir;
        let call = module
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskRun,
                        ..
                    }
                )
            })
            .expect("TaskRun exists");
        let mir::Instruction::CallIntrinsic { arguments, .. } = call else {
            unreachable!();
        };
        arguments[1].type_ = mir::Type::Long;

        let error = validate_module(&module)
            .expect_err("a TaskRun argument with the wrong scalar ABI must be rejected");
        assert!(error.message().contains("malformed TaskRun"));
    }

    #[test]
    fn task_run_rejects_a_reference_bearing_aggregate_argument() {
        let mut module = aster_compiler::compile(
            "public class Box { public Box() {} } \
             public struct Holder { public Box Value; } \
             public int Consume(Holder value) { return 1; } \
             public int Compute() { return 1; } \
             public int Main(Holder input) { return Task.Run(Compute).Wait(); }",
        )
        .expect("source compiles")
        .mir;
        let consume = module
            .functions
            .iter()
            .find(|function| function.name == "Consume")
            .expect("Consume is declared")
            .symbol;
        let main = module
            .functions
            .iter_mut()
            .find(|function| function.name == "Main")
            .expect("Main is declared");
        let input = main.parameters[0].clone();
        let call = main
            .blocks
            .iter_mut()
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskRun,
                        ..
                    }
                )
            })
            .expect("TaskRun exists");
        let mir::Instruction::CallIntrinsic { arguments, .. } = call else {
            unreachable!();
        };
        arguments[0].kind = mir::OperandKind::Function(consume);
        arguments.push(mir::Operand {
            type_: input.type_,
            kind: mir::OperandKind::Copy(mir::Place::Local(input.id)),
        });

        let error = validate_module(&module)
            .expect_err("a reference-bearing task argument must be rejected before codegen");
        assert!(error.message().contains("non-transferable argument type"));
    }

    #[test]
    fn task_wait_all_rejects_mismatched_task_and_result_arrays() {
        let mut module = aster_compiler::compile(
            "public int Compute() { return 1; } \
             public int Main() { Task<int>[] tasks = [Task.Run(Compute)]; return Task.WaitAll(tasks)[0]; }",
        )
        .expect("source compiles")
        .mir;
        let call = module
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskWaitAll,
                        ..
                    }
                )
            })
            .expect("TaskWaitAll exists");
        let mir::Instruction::CallIntrinsic { return_type, .. } = call else {
            unreachable!();
        };
        *return_type = mir::Type::Array(Box::new(mir::Type::Long));

        let error = validate_module(&module)
            .expect_err("Task<int>[] cannot produce a long[] WaitAll result");
        assert!(error.message().contains("malformed TaskWaitAll"));
    }

    #[test]
    fn task_cancel_rejects_a_non_task_receiver() {
        let mut module = aster_compiler::compile(
            "public int Compute() { return 1; } \
             public bool Main() { Task<int> task = Task.Run(Compute); return task.Cancel(); }",
        )
        .expect("source compiles")
        .mir;
        let call = module
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskCancel,
                        ..
                    }
                )
            })
            .expect("TaskCancel exists");
        let mir::Instruction::CallIntrinsic { arguments, .. } = call else {
            unreachable!();
        };
        arguments[0] = mir::Operand {
            type_: mir::Type::Int,
            kind: mir::OperandKind::Constant(mir::Constant::Integer("0".to_owned())),
        };

        let error = validate_module(&module)
            .expect_err("TaskCancel on an int must be rejected before codegen");
        assert!(error.message().contains("malformed TaskCancel"));
    }

    #[test]
    fn task_cancellation_requested_rejects_a_non_bool_result() {
        let mut module = aster_compiler::compile(
            "public bool Main() { return Task.IsCancellationRequested(); }",
        )
        .expect("source compiles")
        .mir;
        let call = module
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskCancellationRequested,
                        ..
                    }
                )
            })
            .expect("TaskCancellationRequested exists");
        let mir::Instruction::CallIntrinsic { return_type, .. } = call else {
            unreachable!();
        };
        *return_type = mir::Type::Int;

        let error = validate_module(&module)
            .expect_err("a non-bool cancellation query must fail before codegen");
        assert!(
            error
                .message()
                .contains("malformed TaskCancellationRequested")
        );
    }

    #[test]
    fn task_wait_rejects_a_non_transferable_result_type() {
        let mut module = aster_compiler::compile(
            "public class Box { public Box() {} } \
             public int Compute() { return 1; } \
             public int Main() { return Task.Run(Compute).Wait(); }",
        )
        .expect("source compiles")
        .mir;
        let box_class = module
            .classes
            .iter()
            .find(|class| class.name == "Box")
            .expect("Box is declared")
            .symbol;
        for function in &mut module.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    if let mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::TaskWait,
                        arguments,
                        return_type,
                        ..
                    } = instruction
                    {
                        arguments[0].type_ = mir::Type::Task(Box::new(mir::Type::Class(box_class)));
                        *return_type = mir::Type::Class(box_class);
                    }
                }
            }
        }

        let error = validate_module(&module)
            .expect_err("a Task<T>.Wait result with arena identity must be rejected");
        assert!(error.message().contains("malformed TaskWait"));
    }

    #[test]
    fn async_store_slot_rejects_a_decimal_value() {
        // `decimal` is rejected here even earlier than the `AsyncStoreSlot`
        // shape check: `validate_operand`'s generic value-type pass already
        // refuses any `decimal` operand anywhere in MIR (see
        // `validate_value_type`), before intrinsic-specific shape validation
        // even runs. The frame slot is still provably never populated with a
        // `decimal`, just via the more fundamental of the two gates.
        let mut module = aster_compiler::compile(
            "public int Compute() { return 1; } \
             public async Task<int> Calculate() { int value = 5; int result = await Task.Run(Compute); return value + result; }",
        )
        .expect("source compiles")
        .mir;
        for function in &mut module.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    if let mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::AsyncStoreSlot,
                        arguments,
                        ..
                    } = instruction
                    {
                        arguments[2].type_ = mir::Type::Decimal;
                    }
                }
            }
        }

        let error = validate_module(&module)
            .expect_err("a decimal async frame slot must be rejected, not silently stored");
        assert!(error.message().contains("cannot execute yet"));
    }

    #[test]
    fn async_load_slot_rejects_a_decimal_result() {
        let mut module = aster_compiler::compile(
            "public int Compute() { return 1; } \
             public async Task<int> Calculate() { int value = 5; int result = await Task.Run(Compute); return value + result; }",
        )
        .expect("source compiles")
        .mir;
        for function in &mut module.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    if let mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::AsyncLoadSlot,
                        return_type,
                        ..
                    } = instruction
                    {
                        *return_type = mir::Type::Decimal;
                    }
                }
            }
        }

        let error = validate_module(&module)
            .expect_err("loading a decimal async frame slot must be rejected");
        assert!(error.message().contains("cannot execute yet"));
    }

    /// Isolates the `is_worker_transferable` shape check itself (as opposed
    /// to the earlier, more general `decimal`-anywhere-in-MIR rejection
    /// exercised above): `string` is backend-executable (a valid MIR value
    /// type, passes `validate_operand` cleanly) but is not worker-
    /// transferable, so only the `AsyncStoreSlot` shape check can catch it.
    #[test]
    fn async_store_slot_rejects_a_string_value() {
        let mut module = aster_compiler::compile(
            "public int Compute() { return 1; } \
             public async Task<int> Calculate() { int value = 5; int result = await Task.Run(Compute); return value + result; }",
        )
        .expect("source compiles")
        .mir;
        for function in &mut module.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    if let mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::AsyncStoreSlot,
                        arguments,
                        ..
                    } = instruction
                    {
                        arguments[2].type_ = mir::Type::String;
                    }
                }
            }
        }

        let error = validate_module(&module)
            .expect_err("a string async frame slot must be rejected (arena identity)");
        assert!(error.message().contains("malformed AsyncStoreSlot"));
    }

    const REDUCE_SOURCE: &str = "public int AddValue(int accumulator, int value) { return accumulator + value; } \
         public int AddPartial(int left, int right) { return left + right; } \
         public class Box { public Box() {} } \
         public Box MakeBox() { return new Box(); } \
         public int Main() { int[] values = [1, 2, 3]; return Parallel.Reduce(values, 0, AddValue, AddPartial); }";

    fn mutate_reduce<F>(source: &str, mut mutate: F) -> mir::Module
    where
        F: FnMut(&mut Option<mir::Place>, &mut [mir::Operand], &mut mir::Type),
    {
        let mut module = aster_compiler::compile(source)
            .expect("source compiles")
            .mir;
        for function in &mut module.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    if let mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::ParallelReduce,
                        destination,
                        arguments,
                        return_type,
                    } = instruction
                    {
                        mutate(destination, arguments, return_type);
                    }
                }
            }
        }
        module
    }

    #[test]
    fn parallel_reduce_rejects_a_nonexistent_accumulate_symbol() {
        let module = mutate_reduce(REDUCE_SOURCE, |_, arguments, _| {
            arguments[2].kind = mir::OperandKind::Function(mir::SymbolId(999_999));
        });
        let error =
            validate_module(&module).expect_err("a symbol absent from the module must be rejected");
        assert!(error.message().contains("malformed ParallelReduce"));
    }

    #[test]
    fn parallel_reduce_rejects_an_accumulate_retargeted_to_a_mismatched_symbol() {
        // Retarget `Accumulate` to `MakeBox`, a real symbol in the module
        // whose actual arity (zero parameters) and return type (`Box`)
        // match neither `Accumulate`'s required shape nor its carried
        // element type: `function_operand_matches` must reject it.
        let compiled = aster_compiler::compile(REDUCE_SOURCE)
            .expect("source compiles")
            .mir;
        let make_box = compiled
            .functions
            .iter()
            .find(|function| function.name == "MakeBox")
            .expect("MakeBox is declared")
            .symbol;
        let mut module = compiled;
        for function in &mut module.functions {
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    if let mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::ParallelReduce,
                        arguments,
                        ..
                    } = instruction
                    {
                        arguments[2].kind = mir::OperandKind::Function(make_box);
                    }
                }
            }
        }

        let error =
            validate_module(&module).expect_err("an arity/signature mismatch must be rejected");
        assert!(error.message().contains("malformed ParallelReduce"));
    }

    #[test]
    fn parallel_reduce_rejects_a_mutated_return_type_with_arena_identity() {
        // `box` is a genuine `Class(Box)`-typed local (a `Copy` operand, not
        // a reinterpreted integer constant), so retargeting `identity` to it
        // isolates the `is_worker_transferable` gate itself rather than
        // tripping the unrelated integer-constant-width check.
        let compiled = aster_compiler::compile(
            "public int AddValue(int accumulator, int value) { return accumulator + value; } \
             public int AddPartial(int left, int right) { return left + right; } \
             public class Box { public Box() {} } \
             public Box MakeBox() { return new Box(); } \
             public int Main() { \
                 Box box = MakeBox(); \
                 int[] values = [1, 2, 3]; \
                 return Parallel.Reduce(values, 0, AddValue, AddPartial); \
             }",
        )
        .expect("source compiles")
        .mir;
        let box_class = compiled
            .classes
            .iter()
            .find(|class| class.name == "Box")
            .expect("Box is declared")
            .symbol;
        let make_box = compiled
            .functions
            .iter()
            .find(|function| function.name == "MakeBox")
            .expect("MakeBox is declared")
            .symbol;
        let mut module = compiled;
        for function in &mut module.functions {
            if function.name != "Main" {
                continue;
            }
            let box_local = function
                .locals
                .iter()
                .find(|local| local.type_ == mir::Type::Class(box_class))
                .expect("`box` local is present")
                .id;
            for block in &mut function.blocks {
                for instruction in &mut block.instructions {
                    if let mir::Instruction::CallIntrinsic {
                        intrinsic: mir::Intrinsic::ParallelReduce,
                        arguments,
                        return_type,
                        ..
                    } = instruction
                    {
                        // Retarget both operators and the declared return
                        // type consistently to `Box`, so only the
                        // `is_worker_transferable` gate (not a mere
                        // signature mismatch) is exercised.
                        *return_type = mir::Type::Class(box_class);
                        arguments[1] = mir::Operand {
                            type_: mir::Type::Class(box_class),
                            kind: mir::OperandKind::Copy(mir::Place::Local(box_local)),
                        };
                        arguments[2].kind = mir::OperandKind::Function(make_box);
                        arguments[2].type_ = mir::Type::Int;
                        arguments[3].kind = mir::OperandKind::Function(make_box);
                        arguments[3].type_ = mir::Type::Class(box_class);
                    }
                }
            }
        }

        let error = validate_module(&module)
            .expect_err("a Parallel.Reduce result with arena identity must be rejected");
        assert!(error.message().contains("malformed ParallelReduce"));
    }

    #[test]
    fn parallel_reduce_rejects_a_mutated_element_type_mismatch() {
        let module = mutate_reduce(REDUCE_SOURCE, |_, arguments, _| {
            // The array's element type (`int`) no longer matches
            // `Accumulate`'s carried element type.
            arguments[2].type_ = mir::Type::Long;
        });
        let error = validate_module(&module)
            .expect_err("an element/parameter type mismatch must be rejected");
        assert!(error.message().contains("malformed ParallelReduce"));
    }

    #[test]
    fn parallel_reduce_rejects_a_mutated_combine_type() {
        let module = mutate_reduce(REDUCE_SOURCE, |_, arguments, _| {
            arguments[3].type_ = mir::Type::Long;
        });
        let error = validate_module(&module).expect_err(
            "a Combine operand type inconsistent with the declared result must be rejected",
        );
        assert!(error.message().contains("malformed ParallelReduce"));
    }

    #[test]
    fn parallel_reduce_rejects_a_missing_destination() {
        let module = mutate_reduce(REDUCE_SOURCE, |destination, _, _| {
            *destination = None;
        });
        let error = validate_module(&module).expect_err(
            "Parallel.Reduce always produces a value; a missing destination is malformed",
        );
        assert!(error.message().contains("malformed ParallelReduce"));
    }

    #[test]
    fn compiler_owned_region_mir_is_valid() {
        validate_module(&owned_region_module()).expect("compiler-owned region is valid");
    }

    #[test]
    fn owned_region_rejects_wrong_id_unknown_local_and_double_exit() {
        let valid = owned_region_module();

        let mut wrong_id = valid.clone();
        let exit = wrong_id
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::OwnedRegionExit { id, .. } => Some(id),
                _ => None,
            })
            .expect("owned exit exists");
        *exit = mir::OwnedRegionId(u32::MAX);
        assert!(
            validate_module(&wrong_id)
                .expect_err("wrong owned id is rejected")
                .message()
                .contains("owned-region exit")
        );

        let mut unknown_local = valid.clone();
        let invalidated = unknown_local
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::OwnedRegionExit { invalidated, .. } => Some(invalidated),
                _ => None,
            })
            .expect("owned exit exists");
        invalidated.push(mir::LocalId(u32::MAX));
        assert!(
            validate_module(&unknown_local)
                .expect_err("unknown invalidated local is rejected")
                .message()
                .contains("owned-region exit")
        );

        let mut double_exit = valid;
        let block = double_exit
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, mir::Instruction::OwnedRegionExit { .. })
                })
            })
            .expect("owned block exists");
        let exit = block
            .instructions
            .iter()
            .find(|instruction| matches!(instruction, mir::Instruction::OwnedRegionExit { .. }))
            .expect("owned exit exists")
            .clone();
        block.instructions.push(exit);
        assert!(
            validate_module(&double_exit)
                .expect_err("double owned exit is rejected")
                .message()
                .contains("without a matching enter")
        );
    }

    #[test]
    fn owned_region_rejects_reclaimed_use_and_missing_producer_owner() {
        let valid = owned_region_module();
        let mut reclaimed_use = valid.clone();
        let function = reclaimed_use
            .functions
            .iter_mut()
            .find(|function| function.name == "Main")
            .expect("Main exists");
        let block = function
            .blocks
            .iter_mut()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, mir::Instruction::OwnedRegionExit { .. })
                })
            })
            .expect("owned block exists");
        let (exit_index, local) = block
            .instructions
            .iter()
            .enumerate()
            .find_map(|(index, instruction)| match instruction {
                mir::Instruction::OwnedRegionExit { invalidated, .. } => {
                    Some((index, invalidated[0]))
                }
                _ => None,
            })
            .expect("owned exit exists");
        let type_ = function
            .locals
            .iter()
            .chain(&function.parameters)
            .find(|candidate| candidate.id == local)
            .expect("invalidated local is declared")
            .type_
            .clone();
        block.instructions.insert(
            exit_index + 1,
            mir::Instruction::Assign {
                target: mir::Place::Local(local),
                value: mir::Rvalue {
                    type_: type_.clone(),
                    kind: mir::RvalueKind::Use(mir::Operand {
                        type_,
                        kind: mir::OperandKind::Copy(mir::Place::Local(local)),
                    }),
                },
            },
        );
        assert!(
            validate_module(&reclaimed_use)
                .expect_err("reclaimed local use is rejected")
                .message()
                .contains("before redefinition")
        );

        let mut no_owner = valid;
        let block = no_owner
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, mir::Instruction::OwnedRegionEnter { .. })
                })
            })
            .expect("owned block exists");
        block.instructions.retain(|instruction| {
            !matches!(
                instruction,
                mir::Instruction::Call {
                    return_type: mir::Type::Array(_),
                    ..
                }
            )
        });
        assert!(
            validate_module(&no_owner)
                .expect_err("owned region without producer is rejected")
                .message()
                .contains("producer call")
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn owned_region_rejects_unrelated_persistent_effects_and_missing_aliases() {
        let valid = owned_region_module();

        let mut persistent_allocation = valid.clone();
        let function = persistent_allocation
            .functions
            .iter_mut()
            .find(|function| function.name == "Main")
            .expect("Main exists");
        let block = function
            .blocks
            .iter_mut()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, mir::Instruction::OwnedRegionExit { .. })
                })
            })
            .expect("owned block exists");
        let (exit_index, destination) = block
            .instructions
            .iter()
            .enumerate()
            .find_map(|(index, instruction)| match instruction {
                mir::Instruction::OwnedRegionExit { invalidated, .. } => {
                    Some((index, invalidated[0]))
                }
                _ => None,
            })
            .expect("owned exit exists");
        block.instructions.insert(
            exit_index,
            mir::Instruction::AllocateArray {
                destination: mir::Place::Local(destination),
                element_type: mir::Type::Int,
                length: mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer("1".to_owned())),
                },
                initialization: mir::ArrayInitialization::Default,
                region: mir::AllocationRegion::Persistent,
            },
        );
        assert!(
            validate_module(&persistent_allocation)
                .expect_err("unrelated Persistent allocation is rejected")
                .message()
                .contains("Persistent effect")
        );

        let mut persistent_call = valid.clone();
        let make = persistent_call
            .functions
            .iter()
            .find(|function| function.name == "Make")
            .expect("Make exists")
            .symbol;
        let function = persistent_call
            .functions
            .iter_mut()
            .find(|function| function.name == "Main")
            .expect("Main exists");
        let block = function
            .blocks
            .iter_mut()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, mir::Instruction::OwnedRegionExit { .. })
                })
            })
            .expect("owned block exists");
        let (exit_index, destination) = block
            .instructions
            .iter()
            .enumerate()
            .find_map(|(index, instruction)| match instruction {
                mir::Instruction::OwnedRegionExit { invalidated, .. } => {
                    Some((index, invalidated[0]))
                }
                _ => None,
            })
            .expect("owned exit exists");
        block.instructions.insert(
            exit_index,
            mir::Instruction::Call {
                destination: Some(mir::Place::Local(destination)),
                function: make,
                arguments: vec![mir::Operand {
                    type_: mir::Type::Int,
                    kind: mir::OperandKind::Constant(mir::Constant::Integer("1".to_owned())),
                }],
                return_type: mir::Type::Array(Box::new(mir::Type::Int)),
            },
        );
        assert!(
            validate_module(&persistent_call)
                .expect_err("transitive Persistent call is rejected")
                .message()
                .contains("Persistent effect")
        );

        let mut missing_alias = valid;
        let invalidated = missing_alias
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find_map(|instruction| match instruction {
                mir::Instruction::OwnedRegionExit { invalidated, .. } => Some(invalidated),
                _ => None,
            })
            .expect("owned exit exists");
        assert!(invalidated.len() > 1);
        invalidated.remove(0);
        assert!(
            validate_module(&missing_alias)
                .expect_err("missing direct alias is rejected")
                .message()
                .contains("owned-region exit")
        );
    }

    /// `AllocateList` has no source syntax yet (List A exposes no
    /// constructor), so every scenario here is hand-built MIR rather than a
    /// mutation of compiled output — the same "adulterated MIR must still be
    /// caught" guarantee the `task_run_rejects_a_non_transferable_result_type`
    /// family exercises above, just with no valid program to start from.
    fn allocate_list_module(declared_type: mir::Type, element_type: mir::Type) -> mir::Module {
        let destination = mir::LocalId(0);
        mir::Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: vec![mir::Function {
                constructor: false,
                symbol: mir::SymbolId(1),
                owner: None,
                name: "Make".to_owned(),
                visibility: mir::Visibility::Public,
                parameters: Vec::new(),
                locals: vec![mir::Local {
                    id: destination,
                    symbol: None,
                    name: "list".to_owned(),
                    type_: declared_type,
                    mutable: true,
                    temporary: false,
                }],
                return_type: mir::Type::Void,
                entry: mir::BasicBlockId(0),
                blocks: vec![mir::BasicBlock {
                    id: mir::BasicBlockId(0),
                    instructions: vec![mir::Instruction::AllocateList {
                        destination: mir::Place::Local(destination),
                        element_type,
                        region: mir::AllocationRegion::Persistent,
                    }],
                    terminator: mir::Terminator::Return(None),
                }],
                temporary_subregion_candidates: Vec::new(),
            }],
        }
    }

    #[test]
    fn allocate_list_of_int_into_a_matching_local_is_valid() {
        let module =
            allocate_list_module(mir::Type::List(Box::new(mir::Type::Int)), mir::Type::Int);
        validate_module(&module).expect("List<int> is a well-formed allocation");
    }

    #[test]
    fn rejects_non_executable_aarm_temporary_subregion_candidates() {
        let mut module =
            allocate_list_module(mir::Type::List(Box::new(mir::Type::Int)), mir::Type::Int);
        let mut unreachable = module.functions[0].clone();
        unreachable.symbol = mir::SymbolId(2);
        unreachable.name = "UnreachableCandidate".to_owned();
        unreachable.temporary_subregion_candidates = vec![mir::TemporarySubregionCandidate {
            id: mir::TemporarySubregionId(0),
            checkpoint: mir::MirPoint {
                block: mir::BasicBlockId(0),
                instruction_boundary: 0,
            },
            rewinds: vec![mir::MirPoint {
                block: mir::BasicBlockId(0),
                instruction_boundary: 1,
            }],
            allocations: vec![mir::MirAllocationSite {
                function: mir::SymbolId(2),
                block: mir::BasicBlockId(0),
                instruction_index: 0,
            }],
        }];
        module.functions.push(unreachable);

        let error = validate_module(&module)
            .expect_err("AARM-5B candidate metadata must fail closed before code generation");
        assert!(
            error
                .message()
                .contains("does not yet support AARM temporary subregion candidates")
        );
    }

    fn executable_subregion_diamond(missing_else_exit: bool) -> mir::Module {
        let array = mir::LocalId(0);
        let allocate = || mir::Instruction::AllocateArray {
            destination: mir::Place::Local(array),
            element_type: mir::Type::Int,
            length: mir::Operand {
                type_: mir::Type::Int,
                kind: mir::OperandKind::Constant(mir::Constant::Integer("1".to_owned())),
            },
            initialization: mir::ArrayInitialization::Default,
            region: mir::AllocationRegion::Temporary,
        };
        mir::Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: vec![mir::Function {
                constructor: false,
                symbol: mir::SymbolId(77),
                owner: None,
                name: "FineDiamond".to_owned(),
                visibility: mir::Visibility::Public,
                parameters: Vec::new(),
                locals: vec![mir::Local {
                    id: array,
                    symbol: None,
                    name: "array".to_owned(),
                    type_: mir::Type::Array(Box::new(mir::Type::Int)),
                    mutable: true,
                    temporary: true,
                }],
                return_type: mir::Type::Void,
                entry: mir::BasicBlockId(0),
                temporary_subregion_candidates: Vec::new(),
                blocks: vec![
                    mir::BasicBlock {
                        id: mir::BasicBlockId(0),
                        instructions: vec![mir::Instruction::TemporarySubregionEnter {
                            id: mir::TemporarySubregionId(0),
                        }],
                        terminator: mir::Terminator::Branch {
                            condition: mir::Operand {
                                type_: mir::Type::Bool,
                                kind: mir::OperandKind::Constant(mir::Constant::Boolean(true)),
                            },
                            then_block: mir::BasicBlockId(1),
                            else_block: mir::BasicBlockId(2),
                        },
                    },
                    mir::BasicBlock {
                        id: mir::BasicBlockId(1),
                        instructions: vec![
                            allocate(),
                            mir::Instruction::TemporarySubregionExit {
                                id: mir::TemporarySubregionId(0),
                            },
                        ],
                        terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
                    },
                    mir::BasicBlock {
                        id: mir::BasicBlockId(2),
                        instructions: if missing_else_exit {
                            vec![allocate()]
                        } else {
                            vec![
                                allocate(),
                                mir::Instruction::TemporarySubregionExit {
                                    id: mir::TemporarySubregionId(0),
                                },
                            ]
                        },
                        terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
                    },
                    mir::BasicBlock {
                        id: mir::BasicBlockId(3),
                        instructions: Vec::new(),
                        terminator: mir::Terminator::Return(None),
                    },
                ],
            }],
        }
    }

    #[test]
    fn executable_acyclic_diamond_requires_a_balanced_exit_on_every_path() {
        validate_module(&executable_subregion_diamond(false))
            .expect("balanced mutually-exclusive fine exits are valid");
        let error = validate_module(&executable_subregion_diamond(true))
            .expect_err("a branch which bypasses its fine exit must fail closed");
        assert!(error.message().contains("inconsistent") || error.message().contains("active"));
    }

    #[test]
    fn allocate_list_rejects_a_destination_declared_as_a_bare_element_type() {
        let module = allocate_list_module(mir::Type::Int, mir::Type::Int);
        let error = validate_module(&module)
            .expect_err("a non-List destination for AllocateList must be rejected");
        assert!(error.message().contains("declared `int`"));
        assert!(error.message().contains("constructs `List<int>`"));
    }

    #[test]
    fn allocate_list_rejects_a_destination_declared_with_a_different_element_type() {
        let module =
            allocate_list_module(mir::Type::List(Box::new(mir::Type::Long)), mir::Type::Int);
        let error = validate_module(&module)
            .expect_err("List<long> destination with a List<int> allocation must be rejected");
        assert!(error.message().contains("declared `List<long>`"));
        assert!(error.message().contains("constructs `List<int>`"));
    }

    #[test]
    fn allocate_list_rejects_a_void_element_type() {
        // The declared-local check (every local's type is validated up front
        // via `validate_value_type`, which now recurses into `List<T>`'s
        // element) rejects this before `AllocateList`'s own
        // `validate_list_element_type` void check ever runs — both exist and
        // either is a correct place to catch it, so this only pins the
        // outcome, not which one fires.
        let module =
            allocate_list_module(mir::Type::List(Box::new(mir::Type::Void)), mir::Type::Void);
        let error = validate_module(&module)
            .expect_err("List<void> is not a value type and must be rejected");
        assert!(error.message().contains("void"));
    }

    #[test]
    fn allocate_list_rejects_a_decimal_element_type() {
        let module = allocate_list_module(
            mir::Type::List(Box::new(mir::Type::Decimal)),
            mir::Type::Decimal,
        );
        let error = validate_module(&module)
            .expect_err("List<decimal> has no runtime representation yet and must be rejected");
        assert!(error.message().contains("decimal"));
    }

    #[test]
    fn allocate_list_rejects_an_unknown_class_element() {
        let unknown_class = mir::Type::Class(mir::SymbolId(999));
        let module = allocate_list_module(
            mir::Type::List(Box::new(unknown_class.clone())),
            unknown_class,
        );
        let error = validate_module(&module)
            .expect_err("a List<T> element class absent from the module must be rejected");
        assert!(error.message().contains("element class is unknown"));
    }

    #[test]
    fn allocate_list_rejects_an_unknown_struct_element() {
        let unknown_struct = mir::Type::User(mir::SymbolId(999));
        let module = allocate_list_module(
            mir::Type::List(Box::new(unknown_struct.clone())),
            unknown_struct,
        );
        let error = validate_module(&module)
            .expect_err("a List<T> element struct absent from the module must be rejected");
        assert!(error.message().contains("element struct is unknown"));
    }

    #[test]
    fn allocate_list_rejects_an_unknown_interface_element() {
        let unknown_interface = mir::Type::Interface(mir::SymbolId(999));
        let module = allocate_list_module(
            mir::Type::List(Box::new(unknown_interface.clone())),
            unknown_interface,
        );
        let error = validate_module(&module)
            .expect_err("a List<T> element interface absent from the module must be rejected");
        assert!(error.message().contains("element interface is unknown"));
    }

    #[test]
    fn allocate_list_rejects_an_unknown_enum_element() {
        let unknown_enum = mir::Type::Enum(mir::SymbolId(999));
        let module = allocate_list_module(
            mir::Type::List(Box::new(unknown_enum.clone())),
            unknown_enum,
        );
        let error = validate_module(&module)
            .expect_err("a List<T> element enum absent from the module must be rejected");
        assert!(error.message().contains("element enum is unknown"));
    }

    #[test]
    fn allocate_list_rejects_a_nested_list_with_a_bad_inner_element() {
        let inner = mir::Type::Decimal;
        let element_type = mir::Type::List(Box::new(inner));
        let declared = mir::Type::List(Box::new(element_type.clone()));
        let module = allocate_list_module(declared, element_type);
        let error = validate_module(&module)
            .expect_err("List<List<decimal>> must be rejected via the inner element check");
        assert!(error.message().contains("decimal"));
    }

    #[test]
    fn allocate_list_accepts_a_nested_list_of_a_well_formed_element() {
        let inner = mir::Type::Int;
        let element_type = mir::Type::List(Box::new(inner));
        let declared = mir::Type::List(Box::new(element_type.clone()));
        let module = allocate_list_module(declared, element_type);
        validate_module(&module).expect("List<List<int>> is a well-formed nested allocation");
    }

    #[test]
    fn allocate_list_accepts_a_known_class_element() {
        let class_symbol = mir::SymbolId(42);
        let mut module = allocate_list_module(
            mir::Type::List(Box::new(mir::Type::Class(class_symbol))),
            mir::Type::Class(class_symbol),
        );
        module.classes.push(mir::ClassDefinition {
            symbol: class_symbol,
            name: "Widget".to_owned(),
            fields: Vec::new(),
        });
        validate_module(&module).expect("List<Widget> with Widget declared is well-formed");
    }

    #[test]
    fn a_function_returning_list_is_rejected_by_the_entry_point_gate() {
        let function = mir::Function {
            constructor: false,
            symbol: mir::SymbolId(1),
            owner: None,
            name: "Main".to_owned(),
            visibility: mir::Visibility::Public,
            parameters: Vec::new(),
            locals: Vec::new(),
            return_type: mir::Type::List(Box::new(mir::Type::Int)),
            entry: mir::BasicBlockId(0),
            blocks: vec![mir::BasicBlock {
                id: mir::BasicBlockId(0),
                instructions: Vec::new(),
                terminator: mir::Terminator::Return(None),
            }],
            temporary_subregion_candidates: Vec::new(),
        };
        let error = validate_invocable_entry(&function, "Main")
            .expect_err("an entry function returning List<T> must be rejected");
        assert!(error.message().contains("returns a List<T>"));
    }

    /// `ListLength` has no source syntax producing it directly through the
    /// helper below (it takes the receiver/result types as raw MIR), so
    /// every scenario is hand-built, mirroring `allocate_list_module` above.
    fn list_length_module(list_type: mir::Type, declared_result_type: mir::Type) -> mir::Module {
        let list_local = mir::LocalId(0);
        let result_local = mir::LocalId(1);
        mir::Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: vec![mir::Function {
                constructor: false,
                symbol: mir::SymbolId(1),
                owner: None,
                name: "Length".to_owned(),
                visibility: mir::Visibility::Public,
                parameters: vec![mir::Local {
                    id: list_local,
                    symbol: None,
                    name: "list".to_owned(),
                    type_: list_type.clone(),
                    mutable: false,
                    temporary: false,
                }],
                locals: vec![mir::Local {
                    id: result_local,
                    symbol: None,
                    name: "result".to_owned(),
                    type_: declared_result_type.clone(),
                    mutable: true,
                    temporary: false,
                }],
                return_type: mir::Type::Void,
                entry: mir::BasicBlockId(0),
                blocks: vec![mir::BasicBlock {
                    id: mir::BasicBlockId(0),
                    instructions: vec![mir::Instruction::Assign {
                        target: mir::Place::Local(result_local),
                        value: mir::Rvalue {
                            type_: declared_result_type,
                            kind: mir::RvalueKind::ListLength(mir::Operand {
                                type_: list_type,
                                kind: mir::OperandKind::Copy(mir::Place::Local(list_local)),
                            }),
                        },
                    }],
                    terminator: mir::Terminator::Return(None),
                }],
                temporary_subregion_candidates: Vec::new(),
            }],
        }
    }

    #[test]
    fn list_length_accepts_a_well_formed_read() {
        let module = list_length_module(mir::Type::List(Box::new(mir::Type::Int)), mir::Type::Int);
        validate_module(&module).expect("List<int>.Length is well-formed");
    }

    #[test]
    fn list_length_rejects_a_non_list_receiver() {
        let module = list_length_module(mir::Type::Int, mir::Type::Int);
        let error = validate_module(&module)
            .expect_err("ListLength on a non-List<T> receiver must be rejected");
        assert!(error.message().contains("non-`List<T>` receiver"));
    }

    #[test]
    fn list_length_rejects_a_non_int_result() {
        let module = list_length_module(mir::Type::List(Box::new(mir::Type::Int)), mir::Type::Long);
        let error = validate_module(&module)
            .expect_err("ListLength must always produce `int`, never `long`");
        assert!(error.message().contains("result type is not `int`"));
    }

    #[test]
    fn list_length_rejects_a_malformed_list_receiver_type() {
        let module = list_length_module(
            mir::Type::List(Box::new(mir::Type::Decimal)),
            mir::Type::Int,
        );
        let error = validate_module(&module)
            .expect_err("List<decimal> has no runtime representation and must be rejected");
        assert!(error.message().contains("decimal"));
    }

    /// `ListAdd` has real source syntax (unlike `AllocateList`/`ListLength`
    /// when their own validation tests were written), but semantic analysis
    /// already prevents a `List<A>`/value-`B` mismatch from ever compiling —
    /// so, like those, every scenario here is hand-built MIR.
    fn list_add_module(list_type: mir::Type, value_type: mir::Type) -> mir::Module {
        let list_local = mir::LocalId(0);
        let value_local = mir::LocalId(1);
        mir::Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: vec![mir::Function {
                constructor: false,
                symbol: mir::SymbolId(1),
                owner: None,
                name: "Add".to_owned(),
                visibility: mir::Visibility::Public,
                parameters: vec![
                    mir::Local {
                        id: list_local,
                        symbol: None,
                        name: "list".to_owned(),
                        type_: list_type.clone(),
                        mutable: false,
                        temporary: false,
                    },
                    mir::Local {
                        id: value_local,
                        symbol: None,
                        name: "value".to_owned(),
                        type_: value_type.clone(),
                        mutable: false,
                        temporary: false,
                    },
                ],
                locals: Vec::new(),
                return_type: mir::Type::Void,
                entry: mir::BasicBlockId(0),
                blocks: vec![mir::BasicBlock {
                    id: mir::BasicBlockId(0),
                    instructions: vec![mir::Instruction::ListAdd {
                        list: mir::Operand {
                            type_: list_type,
                            kind: mir::OperandKind::Copy(mir::Place::Local(list_local)),
                        },
                        value: mir::Operand {
                            type_: value_type,
                            kind: mir::OperandKind::Copy(mir::Place::Local(value_local)),
                        },
                    }],
                    terminator: mir::Terminator::Return(None),
                }],
                temporary_subregion_candidates: Vec::new(),
            }],
        }
    }

    #[test]
    fn list_add_accepts_a_well_formed_call() {
        let module = list_add_module(mir::Type::List(Box::new(mir::Type::Int)), mir::Type::Int);
        validate_module(&module).expect("List<int>.Add(int) is well-formed");
    }

    #[test]
    fn list_add_rejects_a_non_list_receiver() {
        let module = list_add_module(mir::Type::Int, mir::Type::Int);
        let error = validate_module(&module)
            .expect_err("ListAdd on a non-List<T> receiver must be rejected");
        assert!(error.message().contains("receiver is not"));
    }

    #[test]
    fn list_add_rejects_a_mismatched_value_type() {
        let module = list_add_module(mir::Type::List(Box::new(mir::Type::Int)), mir::Type::Long);
        let error = validate_module(&module).expect_err("List<int>.Add(long) must be rejected");
        assert!(error.message().contains("List<int>"));
        assert!(error.message().contains("long"));
    }

    #[test]
    fn list_add_rejects_a_decimal_element() {
        let module = list_add_module(
            mir::Type::List(Box::new(mir::Type::Decimal)),
            mir::Type::Decimal,
        );
        let error =
            validate_module(&module).expect_err("List<decimal> has no runtime representation");
        assert!(error.message().contains("decimal"));
    }

    #[test]
    fn list_add_rejects_an_unknown_class_element() {
        let unknown_class = mir::Type::Class(mir::SymbolId(999));
        let module = list_add_module(
            mir::Type::List(Box::new(unknown_class.clone())),
            unknown_class,
        );
        let error = validate_module(&module)
            .expect_err("a List<T> element class absent from the module must be rejected");
        assert!(error.message().contains("element class is unknown"));
    }

    #[test]
    fn list_add_rejects_a_nested_list_mismatch() {
        let module = list_add_module(
            mir::Type::List(Box::new(mir::Type::List(Box::new(mir::Type::Int)))),
            mir::Type::List(Box::new(mir::Type::Long)),
        );
        let error =
            validate_module(&module).expect_err("List<List<int>>.Add(List<long>) must be rejected");
        assert!(error.message().contains("List<List<int>>"));
    }

    #[test]
    fn list_add_accepts_a_known_class_element() {
        let class_symbol = mir::SymbolId(42);
        let mut module = list_add_module(
            mir::Type::List(Box::new(mir::Type::Class(class_symbol))),
            mir::Type::Class(class_symbol),
        );
        module.classes.push(mir::ClassDefinition {
            symbol: class_symbol,
            name: "Widget".to_owned(),
            fields: Vec::new(),
        });
        validate_module(&module).expect("List<Widget>.Add(Widget) with Widget declared is valid");
    }

    /// `ListGet` has real source syntax, but semantic analysis already
    /// prevents a receiver/index/result mismatch from ever compiling — so,
    /// like `AllocateList`/`ListAdd`, every scenario here is hand-built MIR.
    fn list_get_module(
        list_type: mir::Type,
        index_type: mir::Type,
        declared_result_type: mir::Type,
        element_type: mir::Type,
    ) -> mir::Module {
        let list_local = mir::LocalId(0);
        let index_local = mir::LocalId(1);
        let result_local = mir::LocalId(2);
        mir::Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: vec![mir::Function {
                constructor: false,
                symbol: mir::SymbolId(1),
                owner: None,
                name: "Get".to_owned(),
                visibility: mir::Visibility::Public,
                parameters: vec![
                    mir::Local {
                        id: list_local,
                        symbol: None,
                        name: "list".to_owned(),
                        type_: list_type.clone(),
                        mutable: false,
                        temporary: false,
                    },
                    mir::Local {
                        id: index_local,
                        symbol: None,
                        name: "index".to_owned(),
                        type_: index_type.clone(),
                        mutable: false,
                        temporary: false,
                    },
                ],
                locals: vec![mir::Local {
                    id: result_local,
                    symbol: None,
                    name: "result".to_owned(),
                    type_: declared_result_type,
                    mutable: true,
                    temporary: false,
                }],
                return_type: mir::Type::Void,
                entry: mir::BasicBlockId(0),
                blocks: vec![mir::BasicBlock {
                    id: mir::BasicBlockId(0),
                    instructions: vec![mir::Instruction::ListGet {
                        destination: mir::Place::Local(result_local),
                        list: mir::Operand {
                            type_: list_type,
                            kind: mir::OperandKind::Copy(mir::Place::Local(list_local)),
                        },
                        index: mir::Operand {
                            type_: index_type,
                            kind: mir::OperandKind::Copy(mir::Place::Local(index_local)),
                        },
                        element_type,
                    }],
                    terminator: mir::Terminator::Return(None),
                }],
                temporary_subregion_candidates: Vec::new(),
            }],
        }
    }

    #[test]
    fn list_get_accepts_a_well_formed_call() {
        let module = list_get_module(
            mir::Type::List(Box::new(mir::Type::Int)),
            mir::Type::Int,
            mir::Type::Int,
            mir::Type::Int,
        );
        validate_module(&module).expect("List<int>.Get(int) -> int is well-formed");
    }

    #[test]
    fn list_get_rejects_a_non_list_receiver() {
        let module = list_get_module(
            mir::Type::Int,
            mir::Type::Int,
            mir::Type::Int,
            mir::Type::Int,
        );
        let error = validate_module(&module)
            .expect_err("ListGet on a non-List<T> receiver must be rejected");
        assert!(error.message().contains("constructs `List<int>`"));
    }

    #[test]
    fn list_get_rejects_a_non_int_index() {
        let module = list_get_module(
            mir::Type::List(Box::new(mir::Type::Int)),
            mir::Type::Long,
            mir::Type::Int,
            mir::Type::Int,
        );
        let error =
            validate_module(&module).expect_err("ListGet index must be `int`, never `long`");
        assert!(error.message().contains("index is not `int`"));
    }

    #[test]
    fn list_get_rejects_a_destination_type_mismatch() {
        let module = list_get_module(
            mir::Type::List(Box::new(mir::Type::Int)),
            mir::Type::Int,
            mir::Type::Long,
            mir::Type::Int,
        );
        let error = validate_module(&module).expect_err(
            "a destination declared `long` receiving a `List<int>.Get` must be rejected",
        );
        assert!(error.message().contains("declared `long`"));
        assert!(error.message().contains("produces `int`"));
    }

    #[test]
    fn list_get_rejects_a_decimal_element() {
        let module = list_get_module(
            mir::Type::List(Box::new(mir::Type::Decimal)),
            mir::Type::Int,
            mir::Type::Decimal,
            mir::Type::Decimal,
        );
        let error =
            validate_module(&module).expect_err("List<decimal> has no runtime representation");
        assert!(error.message().contains("decimal"));
    }

    #[test]
    fn list_get_rejects_an_unknown_class_element() {
        let unknown_class = mir::Type::Class(mir::SymbolId(999));
        let module = list_get_module(
            mir::Type::List(Box::new(unknown_class.clone())),
            mir::Type::Int,
            unknown_class.clone(),
            unknown_class,
        );
        let error = validate_module(&module)
            .expect_err("a List<T> element class absent from the module must be rejected");
        assert!(error.message().contains("element class is unknown"));
    }

    #[test]
    fn list_get_rejects_a_nested_list_mismatch() {
        let module = list_get_module(
            mir::Type::List(Box::new(mir::Type::List(Box::new(mir::Type::Int)))),
            mir::Type::Int,
            mir::Type::List(Box::new(mir::Type::Long)),
            mir::Type::List(Box::new(mir::Type::Long)),
        );
        let error = validate_module(&module)
            .expect_err("List<List<int>>.Get() must not produce List<long>");
        assert!(error.message().contains("List<List<int>>"));
    }

    #[test]
    fn list_get_accepts_a_known_class_element() {
        let class_symbol = mir::SymbolId(42);
        let mut module = list_get_module(
            mir::Type::List(Box::new(mir::Type::Class(class_symbol))),
            mir::Type::Int,
            mir::Type::Class(class_symbol),
            mir::Type::Class(class_symbol),
        );
        module.classes.push(mir::ClassDefinition {
            symbol: class_symbol,
            name: "Widget".to_owned(),
            fields: Vec::new(),
        });
        validate_module(&module).expect("List<Widget>.Get(int) with Widget declared is valid");
    }

    /// `ListRemoveAt` has real source syntax, but semantic analysis already
    /// prevents a receiver/index mismatch from ever compiling — so, like
    /// `AllocateList`/`ListAdd`/`ListGet`, every scenario here is hand-built
    /// MIR.
    fn list_remove_at_module(list_type: mir::Type, index_type: mir::Type) -> mir::Module {
        let list_local = mir::LocalId(0);
        let index_local = mir::LocalId(1);
        mir::Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: vec![mir::Function {
                constructor: false,
                symbol: mir::SymbolId(1),
                owner: None,
                name: "RemoveAt".to_owned(),
                visibility: mir::Visibility::Public,
                parameters: vec![
                    mir::Local {
                        id: list_local,
                        symbol: None,
                        name: "list".to_owned(),
                        type_: list_type.clone(),
                        mutable: false,
                        temporary: false,
                    },
                    mir::Local {
                        id: index_local,
                        symbol: None,
                        name: "index".to_owned(),
                        type_: index_type.clone(),
                        mutable: false,
                        temporary: false,
                    },
                ],
                locals: Vec::new(),
                return_type: mir::Type::Void,
                entry: mir::BasicBlockId(0),
                blocks: vec![mir::BasicBlock {
                    id: mir::BasicBlockId(0),
                    instructions: vec![mir::Instruction::ListRemoveAt {
                        list: mir::Operand {
                            type_: list_type,
                            kind: mir::OperandKind::Copy(mir::Place::Local(list_local)),
                        },
                        index: mir::Operand {
                            type_: index_type,
                            kind: mir::OperandKind::Copy(mir::Place::Local(index_local)),
                        },
                    }],
                    terminator: mir::Terminator::Return(None),
                }],
                temporary_subregion_candidates: Vec::new(),
            }],
        }
    }

    #[test]
    fn list_remove_at_accepts_a_well_formed_call() {
        let module =
            list_remove_at_module(mir::Type::List(Box::new(mir::Type::Int)), mir::Type::Int);
        validate_module(&module).expect("List<int>.RemoveAt(int) is well-formed");
    }

    #[test]
    fn list_remove_at_rejects_a_non_list_receiver() {
        let module = list_remove_at_module(mir::Type::Int, mir::Type::Int);
        let error = validate_module(&module)
            .expect_err("ListRemoveAt on a non-List<T> receiver must be rejected");
        assert!(error.message().contains("receiver is not"));
    }

    #[test]
    fn list_remove_at_rejects_a_non_int_index() {
        let module =
            list_remove_at_module(mir::Type::List(Box::new(mir::Type::Int)), mir::Type::Long);
        let error =
            validate_module(&module).expect_err("ListRemoveAt index must be `int`, never `long`");
        assert!(error.message().contains("index is not `int`"));
    }

    #[test]
    fn list_remove_at_rejects_a_decimal_element() {
        let module = list_remove_at_module(
            mir::Type::List(Box::new(mir::Type::Decimal)),
            mir::Type::Int,
        );
        let error =
            validate_module(&module).expect_err("List<decimal> has no runtime representation");
        assert!(error.message().contains("decimal"));
    }

    #[test]
    fn list_remove_at_rejects_an_unknown_class_element() {
        let unknown_class = mir::Type::Class(mir::SymbolId(999));
        let module =
            list_remove_at_module(mir::Type::List(Box::new(unknown_class)), mir::Type::Int);
        let error = validate_module(&module)
            .expect_err("a List<T> element class absent from the module must be rejected");
        assert!(error.message().contains("element class is unknown"));
    }

    #[test]
    fn list_remove_at_rejects_a_nested_list_element_with_a_bad_inner_element() {
        let module = list_remove_at_module(
            mir::Type::List(Box::new(mir::Type::List(Box::new(mir::Type::Decimal)))),
            mir::Type::Int,
        );
        let error = validate_module(&module)
            .expect_err("List<List<decimal>> must be rejected via the inner element check");
        assert!(error.message().contains("decimal"));
    }

    #[test]
    fn list_remove_at_accepts_a_known_class_element() {
        let class_symbol = mir::SymbolId(42);
        let mut module = list_remove_at_module(
            mir::Type::List(Box::new(mir::Type::Class(class_symbol))),
            mir::Type::Int,
        );
        module.classes.push(mir::ClassDefinition {
            symbol: class_symbol,
            name: "Widget".to_owned(),
            fields: Vec::new(),
        });
        validate_module(&module).expect("List<Widget>.RemoveAt(int) with Widget declared is valid");
    }
}
