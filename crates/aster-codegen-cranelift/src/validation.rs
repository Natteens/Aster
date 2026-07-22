use super::{BackendError, HashMap, HashSet, integer_constant_bits, mir, primitive, type_name};

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
    Ok(())
}

pub(super) fn validate_module(module: &mir::Module) -> Result<(), BackendError> {
    let signatures = module
        .functions
        .iter()
        .map(|function| (function.symbol, function))
        .collect::<HashMap<_, _>>();
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
    for function in &module.functions {
        validate_function(
            function,
            &signatures,
            &classes,
            &structs,
            &interfaces,
            &enums,
            &interface_methods,
            &implementations,
        )?;
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
    classes: &HashSet<mir::SymbolId>,
    structs: &HashSet<mir::SymbolId>,
    interfaces: &HashSet<mir::SymbolId>,
    enums: &HashSet<mir::SymbolId>,
    interface_methods: &InterfaceMethods<'_>,
    implementations: &HashSet<(mir::SymbolId, mir::SymbolId)>,
) -> Result<(), BackendError> {
    if function
        .owner
        .is_some_and(|owner| !classes.contains(&owner))
    {
        return Err(unsupported(&function.name, "struct methods"));
    }
    validate_return_type(&function.return_type, &function.name)?;
    for parameter in &function.parameters {
        validate_value_type(&parameter.type_, &function.name)?;
    }
    for local in &function.locals {
        validate_value_type(&local.type_, &function.name)?;
    }
    let locals = function
        .locals
        .iter()
        .map(|local| (local.id, local.type_.clone()))
        .collect::<HashMap<_, _>>();
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
        validate_terminator(&block.terminator, &function.name)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
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
        mir::Instruction::Assign { target, value } => {
            validate_place(target, function_name)?;
            validate_rvalue(value, function_name, implementations)
        }
        mir::Instruction::Call {
            destination,
            function,
            arguments,
            return_type,
        } => {
            if let Some(destination) = destination {
                validate_place(destination, function_name)?;
            }
            validate_return_type(return_type, function_name)?;
            for argument in arguments {
                validate_operand(argument, function_name)?;
            }
            let called = signatures.get(function).ok_or_else(|| {
                BackendError::new(format!(
                    "function `{function_name}` calls an unsupported external function with symbol {}",
                    function.0
                ))
            })?;
            if called.owner.is_some_and(|owner| !classes.contains(&owner)) {
                return Err(unsupported(function_name, "struct method calls"));
            }
            Ok(())
        }
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
        ),
        mir::Instruction::AllocateArray {
            destination,
            element_type,
            length,
            ..
        } => validate_allocate_array(destination, element_type, length, function_name),
        mir::Instruction::AllocateObject {
            destination, class, ..
        } => {
            validate_place(destination, function_name)?;
            if classes.contains(class) {
                Ok(())
            } else {
                Err(unsupported(function_name, "allocation of a non-class type"))
            }
        }
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
        mir::Instruction::ListAdd { list, value } => validate_list_add(
            list,
            value,
            function_name,
            classes,
            structs,
            interfaces,
            enums,
        ),
    }
}

fn validate_allocate_array(
    destination: &mir::Place,
    element_type: &mir::Type,
    length: &mir::Operand,
    function_name: &str,
) -> Result<(), BackendError> {
    validate_place(destination, function_name)?;
    validate_value_type(element_type, function_name)?;
    validate_operand(length, function_name)
}

fn validate_call_intrinsic(
    destination: Option<&mir::Place>,
    intrinsic: mir::Intrinsic,
    arguments: &[mir::Operand],
    return_type: &mir::Type,
    function_name: &str,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
) -> Result<(), BackendError> {
    if let Some(destination) = destination {
        validate_place(destination, function_name)?;
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
                && matches!(
                    arguments,
                    [value] if matches!(value.type_, mir::Type::Float | mir::Type::Double)
                )
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
        mir::Intrinsic::ReportRuntimeError(_) => {
            destination.is_none() && return_type == &mir::Type::Void && arguments.is_empty()
        }
        mir::Intrinsic::TaskRun => {
            destination.is_some()
                && matches!(
                    (return_type, arguments),
                    (mir::Type::Task(result), [argument])
                        if matches!(argument.kind, mir::OperandKind::Function(_))
                            && argument.type_ == **result
                            && is_worker_transferable(result)
                            && function_operand_matches(argument, &[], result, signatures)
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
                    [handle, inner]
                        if handle.type_ == mir::Type::Long
                            && matches!(inner.kind, mir::OperandKind::Function(_))
                            && function_operand_matches(inner, &[], &inner.type_, signatures)
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
        Err(BackendError::new(format!(
            "function `{function_name}` contains a malformed {intrinsic:?} runtime intrinsic"
        )))
    }
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

fn validate_rvalue(
    value: &mir::Rvalue,
    function_name: &str,
    implementations: &HashSet<(mir::SymbolId, mir::SymbolId)>,
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
                validate_operand(&field.value, function_name)?;
            }
            Ok(())
        }
        mir::RvalueKind::ArrayLength(array) => validate_operand(array, function_name),
        mir::RvalueKind::ListLength(list) => {
            validate_operand(list, function_name)?;
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
            validate_operand(object, function_name)
        }
        mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => validate_operand(operand, function_name),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            validate_operand(left, function_name)?;
            validate_operand(right, function_name)
        }
    }
}

fn validate_terminator(
    terminator: &mir::Terminator,
    function_name: &str,
) -> Result<(), BackendError> {
    match terminator {
        mir::Terminator::Branch { condition, .. } => validate_operand(condition, function_name),
        mir::Terminator::Return(Some(value)) => validate_operand(value, function_name),
        mir::Terminator::Goto(_)
        | mir::Terminator::Return(None)
        | mir::Terminator::End
        | mir::Terminator::Unreachable => Ok(()),
    }
}

fn validate_operand(operand: &mir::Operand, function_name: &str) -> Result<(), BackendError> {
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
        mir::OperandKind::Copy(place) => validate_place(place, function_name),
        mir::OperandKind::Function(_) => Err(unsupported(function_name, "function values")),
    }
}

fn validate_place(place: &mir::Place, function_name: &str) -> Result<(), BackendError> {
    match place {
        mir::Place::Local(_) => Ok(()),
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            validate_place(base, function_name)
        }
        mir::Place::Index {
            array,
            index,
            element_type,
        } => {
            validate_operand(array, function_name)?;
            validate_operand(index, function_name)?;
            validate_value_type(element_type, function_name)
        }
        mir::Place::ObjectField { object, .. } => validate_operand(object, function_name),
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
}
