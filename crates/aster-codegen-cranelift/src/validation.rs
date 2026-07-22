use super::{BackendError, HashMap, HashSet, integer_constant_bits, mir, type_name};

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
    let (interface_methods, implementations) =
        validate_interface_metadata(module, &signatures, &classes)?;
    for function in &module.functions {
        validate_function(
            function,
            &signatures,
            &classes,
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

fn validate_function(
    function: &mir::Function,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    classes: &HashSet<mir::SymbolId>,
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
    for block in &function.blocks {
        for instruction in &block.instructions {
            validate_instruction(
                instruction,
                &function.name,
                signatures,
                classes,
                interface_methods,
                implementations,
            )?;
        }
        validate_terminator(&block.terminator, &function.name)?;
    }
    Ok(())
}

fn validate_instruction(
    instruction: &mir::Instruction,
    function_name: &str,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    classes: &HashSet<mir::SymbolId>,
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
        } => {
            if let Some(destination) = destination {
                validate_place(destination, function_name)?;
            }
            validate_return_type(return_type, function_name)?;
            for argument in arguments {
                // Spawn-style intrinsics (`Task.Run`, `AsyncSpawn`,
                // `AsyncSpawnInner`, `Parallel*`) carry a resolved function
                // reference as an `OperandKind::Function`, which the generic
                // `validate_operand` rejects. Validate only its value type;
                // `validate_intrinsic_shape` below checks the full shape.
                if matches!(argument.kind, mir::OperandKind::Function(_)) {
                    validate_value_type(&argument.type_, function_name)?;
                } else {
                    validate_operand(argument, function_name)?;
                }
            }
            validate_intrinsic_shape(
                destination.as_ref(),
                *intrinsic,
                arguments,
                return_type,
                function_name,
                signatures,
            )?;
            Ok(())
        }
        mir::Instruction::AllocateArray {
            destination,
            element_type,
            length,
            ..
        } => {
            validate_place(destination, function_name)?;
            validate_value_type(element_type, function_name)?;
            validate_operand(length, function_name)
        }
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
                            && function_operand_matches(argument, &[], result, signatures)
                )
        }
        mir::Intrinsic::TaskWait => {
            destination.is_some()
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
                            && is_transferable_scalar(&value.type_)
                )
        }
        mir::Intrinsic::AsyncLoadSlot => {
            destination.is_some()
                && is_transferable_scalar(return_type)
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
                && is_transferable_scalar(return_type)
                && matches!(arguments, [handle] if handle.type_ == mir::Type::Long)
        }
        mir::Intrinsic::AsyncSetResult => {
            destination.is_none()
                && *return_type == mir::Type::Void
                && matches!(
                    arguments,
                    [handle, value]
                        if handle.type_ == mir::Type::Long && is_transferable_scalar(&value.type_)
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
                            && is_transferable_scalar(&body.type_)
                            && function_operand_matches(
                                body,
                                std::slice::from_ref(&body.type_),
                                &mir::Type::Void,
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

fn is_transferable_scalar(type_: &mir::Type) -> bool {
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
    )
}

fn executable_value_type(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::SByte
            | mir::Type::Byte
            | mir::Type::Short
            | mir::Type::UShort
            | mir::Type::Int
            | mir::Type::UInt
            | mir::Type::Long
            | mir::Type::ULong
            | mir::Type::Float
            | mir::Type::Double
            | mir::Type::Bool
            | mir::Type::Char
            | mir::Type::String
    )
}

fn validate_value_type(type_: &mir::Type, function_name: &str) -> Result<(), BackendError> {
    if let mir::Type::Array(element) = type_ {
        if matches!(**element, mir::Type::Array(_)) {
            return Err(unsupported(function_name, "nested arrays"));
        }
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
}
