//! Backend-neutral MIR validation: nominal type identity and typed operations.
//!
//! These checks depend only on MIR. Backends run them before code generation and
//! keep their own checks for what they can lower (ABI shapes, supported
//! operations). Compiler passes may run them after each transformation.

#[doc(hidden)]
pub mod executable;
pub mod ownership;

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate as mir;
use crate::type_name;

/// A MIR validation outcome. `Invalid` is a broken MIR invariant, which no
/// compiler pass may produce. `Unsupported` is a well-formed construct that no
/// backend can execute yet; its message is phrased by the backend. `Rejected`
/// is well-formed MIR that an execution policy refuses, such as a host
/// operation reachable from a worker; the message is final.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationError {
    Invalid(String),
    Unsupported { function: String, feature: String },
    Rejected(String),
}

impl ValidationError {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    fn unsupported(function_name: &str, feature: &str) -> Self {
        Self::Unsupported {
            function: function_name.to_owned(),
            feature: feature.to_owned(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::Rejected(message) => formatter.write_str(message),
            Self::Unsupported { function, feature } => {
                write!(formatter, "unsupported {feature} in function `{function}`")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validates the declared types and every typed operation of one function
/// against the module's nominal type index.
///
/// # Errors
///
/// Returns the first violation found.
#[allow(clippy::implicit_hasher)]
pub fn validate_function_types(
    function: &mir::Function,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    index: &MirTypeIndex,
) -> Result<(), ValidationError> {
    index.validate_type(&function.return_type, &function.name)?;
    for local in function.parameters.iter().chain(&function.locals) {
        index.validate_type(&local.type_, &function.name)?;
    }
    let locals = function
        .parameters
        .iter()
        .chain(&function.locals)
        .map(|local| (local.id, local.type_.clone()))
        .collect::<HashMap<_, _>>();
    validate_typed_function(function, &locals, signatures, index)
}

/// Builds the type index and validates every function's types. Duplicate
/// function symbols are an error.
///
/// # Errors
///
/// Returns the first violation found.
pub fn validate_types(module: &mir::Module) -> Result<(), ValidationError> {
    let index = MirTypeIndex::build(module)?;
    let mut signatures = HashMap::new();
    for function in &module.functions {
        if signatures.insert(function.symbol, function).is_some() {
            return Err(ValidationError::invalid(format!(
                "duplicate function symbol {:?} in MIR",
                function.symbol
            )));
        }
    }
    for function in &module.functions {
        validate_function_types(function, &signatures, &index)?;
    }
    Ok(())
}

#[derive(Default)]
pub struct MirTypeIndex {
    kinds: HashMap<mir::SymbolId, NominalKind>,
    struct_fields: HashMap<(mir::SymbolId, mir::SymbolId), mir::Type>,
    class_fields: HashMap<(mir::SymbolId, mir::SymbolId), mir::Type>,
    enum_cases: HashMap<mir::SymbolId, mir::SymbolId>,
    enum_fields: HashMap<(mir::SymbolId, mir::SymbolId), mir::Type>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NominalKind {
    Struct,
    Class,
    Interface,
    Enum,
}

impl MirTypeIndex {
    /// Indexes every nominal declaration and its fields.
    ///
    /// # Errors
    ///
    /// Returns an error for a duplicate nominal symbol or a field symbol
    /// shared between owners.
    pub fn build(module: &mir::Module) -> Result<Self, ValidationError> {
        let mut index = Self::default();
        // Backends index fields by their module-wide symbol identity. Reject
        // collisions here, before a layout could silently replace one owner's
        // field metadata.
        let mut field_symbols = HashSet::new();
        for definition in &module.structs {
            index.insert_type(definition.symbol, NominalKind::Struct)?;
            for field in &definition.fields {
                if !field_symbols.insert(field.symbol) {
                    return Err(ValidationError::invalid("duplicate field symbol in MIR"));
                }
                if index
                    .struct_fields
                    .insert((definition.symbol, field.symbol), field.type_.clone())
                    .is_some()
                {
                    return Err(ValidationError::invalid(
                        "duplicate struct field symbol in MIR",
                    ));
                }
            }
        }
        for definition in &module.classes {
            index.insert_type(definition.symbol, NominalKind::Class)?;
            for field in &definition.fields {
                if !field_symbols.insert(field.symbol) {
                    return Err(ValidationError::invalid("duplicate field symbol in MIR"));
                }
                if index
                    .class_fields
                    .insert((definition.symbol, field.symbol), field.type_.clone())
                    .is_some()
                {
                    return Err(ValidationError::invalid(
                        "duplicate class field symbol in MIR",
                    ));
                }
            }
        }
        for definition in &module.interfaces {
            index.insert_type(definition.symbol, NominalKind::Interface)?;
        }
        for definition in &module.enums {
            index.insert_type(definition.symbol, NominalKind::Enum)?;
            for case in &definition.cases {
                if index
                    .enum_cases
                    .insert(case.symbol, definition.symbol)
                    .is_some()
                {
                    return Err(ValidationError::invalid(
                        "duplicate enum case symbol in MIR",
                    ));
                }
                for field in &case.fields {
                    if !field_symbols.insert(field.symbol) {
                        return Err(ValidationError::invalid("duplicate field symbol in MIR"));
                    }
                    if index
                        .enum_fields
                        .insert((case.symbol, field.symbol), field.type_.clone())
                        .is_some()
                    {
                        return Err(ValidationError::invalid(
                            "duplicate enum field symbol in MIR",
                        ));
                    }
                }
            }
        }
        Ok(index)
    }

    fn insert_type(
        &mut self,
        symbol: mir::SymbolId,
        kind: NominalKind,
    ) -> Result<(), ValidationError> {
        if self.kinds.insert(symbol, kind).is_some() {
            Err(ValidationError::invalid(
                "duplicate nominal type symbol in MIR",
            ))
        } else {
            Ok(())
        }
    }

    fn validate_type(&self, type_: &mir::Type, function_name: &str) -> Result<(), ValidationError> {
        let valid = match type_ {
            mir::Type::User(symbol) => self.kinds.get(symbol) == Some(&NominalKind::Struct),
            mir::Type::Class(symbol) => self.kinds.get(symbol) == Some(&NominalKind::Class),
            mir::Type::Interface(symbol) => self.kinds.get(symbol) == Some(&NominalKind::Interface),
            mir::Type::Enum(symbol) => self.kinds.get(symbol) == Some(&NominalKind::Enum),
            mir::Type::Array(element) | mir::Type::Task(element) | mir::Type::List(element) => {
                return self.validate_type(element, function_name);
            }
            mir::Type::Dictionary(key, value) => {
                self.validate_type(key, function_name)?;
                return self.validate_type(value, function_name);
            }
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(ValidationError::invalid(format!(
                "function `{function_name}` references an unknown or mismatched nominal type"
            )))
        }
    }
}

fn validate_typed_function(
    function: &mir::Function,
    locals: &HashMap<mir::LocalId, mir::Type>,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    types: &MirTypeIndex,
) -> Result<(), ValidationError> {
    for block in &function.blocks {
        for instruction in &block.instructions {
            validate_typed_instruction(instruction, &function.name, locals, signatures, types)?;
        }
        match &block.terminator {
            mir::Terminator::Branch { condition, .. } => {
                validate_typed_operand(condition, &function.name, locals, signatures, types)?;
            }
            mir::Terminator::Return(Some(value)) => {
                validate_typed_operand(value, &function.name, locals, signatures, types)?;
                if value.type_ != function.return_type {
                    return Err(ValidationError::invalid(format!(
                        "function `{}` returns `{}` from a function declared `{}`",
                        function.name,
                        type_name(&value.type_),
                        type_name(&function.return_type)
                    )));
                }
            }
            mir::Terminator::Return(None) if function.return_type != mir::Type::Void => {
                return Err(ValidationError::invalid(format!(
                    "function `{}` returns no value from a non-void function",
                    function.name
                )));
            }
            mir::Terminator::Goto(_)
            | mir::Terminator::Return(None)
            | mir::Terminator::End
            | mir::Terminator::Unreachable => {}
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_typed_instruction(
    instruction: &mir::Instruction,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    types: &MirTypeIndex,
) -> Result<(), ValidationError> {
    let place =
        |value: &mir::Place| validate_typed_place(value, function_name, locals, signatures, types);
    let operand = |value: &mir::Operand| {
        validate_typed_operand(value, function_name, locals, signatures, types)
    };
    let destination = |value: &mir::Place, expected: &mir::Type| -> Result<(), ValidationError> {
        let actual = place(value)?;
        if &actual == expected {
            Ok(())
        } else {
            Err(ValidationError::invalid(format!(
                "function `{function_name}` stores `{}` into a destination declared `{}`",
                type_name(expected),
                type_name(&actual)
            )))
        }
    };

    match instruction {
        mir::Instruction::OwnedRegionEnter { .. }
        | mir::Instruction::OwnedRegionExit { .. }
        | mir::Instruction::TemporarySubregionEnter { .. }
        | mir::Instruction::TemporarySubregionExit { .. } => Ok(()),
        mir::Instruction::Assign { target, value } => {
            let target_type = place(target)?;
            validate_typed_rvalue(value, function_name, locals, signatures, types)?;
            if target_type == value.type_ {
                Ok(())
            } else {
                Err(ValidationError::invalid(format!(
                    "function `{function_name}` assigns `{}` into a place declared `{}`",
                    type_name(&value.type_),
                    type_name(&target_type)
                )))
            }
        }
        mir::Instruction::Call {
            destination: result,
            arguments,
            return_type,
            ..
        }
        | mir::Instruction::ForeignCall {
            destination: result,
            arguments,
            return_type,
            ..
        }
        | mir::Instruction::CallIntrinsic {
            destination: result,
            arguments,
            return_type,
            ..
        } => {
            for argument in arguments {
                operand(argument)?;
            }
            if let Some(result) = result {
                destination(result, return_type)?;
            }
            Ok(())
        }
        mir::Instruction::CallInterface {
            destination: result,
            receiver,
            arguments,
            return_type,
            ..
        } => {
            operand(receiver)?;
            for argument in arguments {
                operand(argument)?;
            }
            if let Some(result) = result {
                destination(result, return_type)?;
            }
            Ok(())
        }
        mir::Instruction::AllocateArray {
            destination: result,
            element_type,
            length,
            ..
        } => {
            operand(length)?;
            destination(result, &mir::Type::Array(Box::new(element_type.clone())))
        }
        mir::Instruction::AllocateObject {
            destination: result,
            class,
            ..
        }
        | mir::Instruction::AllocateStringBuilder {
            destination: result,
            class,
            ..
        } => destination(result, &mir::Type::Class(*class)),
        mir::Instruction::AllocateList {
            destination: result,
            element_type,
            ..
        } => destination(result, &mir::Type::List(Box::new(element_type.clone()))),
        mir::Instruction::AllocateDictionary {
            destination: result,
            key_type,
            value_type,
            ..
        } => destination(
            result,
            &mir::Type::Dictionary(Box::new(key_type.clone()), Box::new(value_type.clone())),
        ),
        mir::Instruction::StringBuilderAppend { builder, value, .. } => {
            operand(builder)?;
            operand(value)
        }
        mir::Instruction::StringBuilderToString {
            destination: result,
            builder,
            ..
        } => {
            operand(builder)?;
            destination(result, &mir::Type::String)
        }
        mir::Instruction::DictionaryAdd {
            destination: result,
            dictionary,
            key,
            value,
        }
        | mir::Instruction::DictionarySet {
            destination: result,
            dictionary,
            key,
            value,
        } => {
            operand(dictionary)?;
            operand(key)?;
            operand(value)?;
            destination(result, &mir::Type::Bool)
        }
        mir::Instruction::DictionaryTryGet {
            destination: result,
            dictionary,
            key,
            ..
        }
        | mir::Instruction::DictionaryContainsKey {
            destination: result,
            dictionary,
            key,
        }
        | mir::Instruction::DictionaryRemove {
            destination: result,
            dictionary,
            key,
        } => {
            operand(dictionary)?;
            operand(key)?;
            place(result).map(|_| ())
        }
        mir::Instruction::DictionaryEntries {
            destination: result,
            dictionary,
            key_type,
            value_type,
            entry_type,
            ..
        } => {
            operand(dictionary)?;
            let expected_dictionary =
                mir::Type::Dictionary(Box::new(key_type.clone()), Box::new(value_type.clone()));
            if dictionary.type_ != expected_dictionary {
                return Err(ValidationError::invalid(format!(
                    "function `{function_name}` has Dictionary entries metadata that does not match its receiver"
                )));
            }
            destination(result, &mir::Type::Array(Box::new(entry_type.clone())))
        }
        mir::Instruction::DictionaryClear { dictionary } => operand(dictionary),
        mir::Instruction::DictionaryKeys {
            destination: result,
            dictionary,
            key_type,
            ..
        } => {
            operand(dictionary)?;
            destination(result, &mir::Type::Array(Box::new(key_type.clone())))
        }
        mir::Instruction::DictionaryValues {
            destination: result,
            dictionary,
            value_type,
            ..
        } => {
            operand(dictionary)?;
            destination(result, &mir::Type::Array(Box::new(value_type.clone())))
        }
        mir::Instruction::ListAdd { list, value } => {
            operand(list)?;
            operand(value)
        }
        mir::Instruction::ListGet {
            destination: result,
            list,
            index,
            element_type,
        } => {
            operand(list)?;
            operand(index)?;
            destination(result, element_type)
        }
        mir::Instruction::ListRemoveAt { list, index } => {
            operand(list)?;
            operand(index)
        }
        mir::Instruction::ListSet { list, index, value } => {
            operand(list)?;
            operand(index)?;
            operand(value)
        }
        mir::Instruction::ListClear { list } => operand(list),
        mir::Instruction::ListToArray {
            destination: result,
            list,
            element_type,
            ..
        } => {
            operand(list)?;
            destination(result, &mir::Type::Array(Box::new(element_type.clone())))
        }
        mir::Instruction::StringDecodeNext {
            string,
            cursor,
            char_destination,
            next_cursor_destination,
            ok_destination,
        } => {
            operand(string)?;
            operand(cursor)?;
            destination(char_destination, &mir::Type::Char)?;
            destination(next_cursor_destination, &mir::Type::Int)?;
            destination(ok_destination, &mir::Type::Bool)
        }
        mir::Instruction::Retain { value } | mir::Instruction::Release { value } => {
            operand(value)?;
            if matches!(
                value.type_,
                mir::Type::Class(_)
                    | mir::Type::Interface(_)
                    | mir::Type::Array(_)
                    | mir::Type::String
            ) {
                Ok(())
            } else {
                Err(ValidationError::invalid(format!(
                    "function `{function_name}` counts a non-reference value of type `{}`",
                    type_name(&value.type_)
                )))
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
fn validate_typed_rvalue(
    value: &mir::Rvalue,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    types: &MirTypeIndex,
) -> Result<(), ValidationError> {
    types.validate_type(&value.type_, function_name)?;
    let operand = |value: &mir::Operand| {
        validate_typed_operand(value, function_name, locals, signatures, types)
    };
    match &value.kind {
        mir::RvalueKind::Use(input) => {
            operand(input)?;
            ensure_same_type(&value.type_, &input.type_, function_name, "copy")
        }
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            for field in fields {
                operand(&field.value)?;
            }
            Ok(())
        }
        mir::RvalueKind::Discriminant(input) => {
            operand(input)?;
            if matches!(input.type_, mir::Type::Enum(_)) && value.type_ == mir::Type::UInt {
                Ok(())
            } else {
                Err(ValidationError::invalid(format!(
                    "function `{function_name}` has a malformed enum discriminant"
                )))
            }
        }
        mir::RvalueKind::ArrayLength(input) => {
            operand(input)?;
            if matches!(input.type_, mir::Type::Array(_)) && value.type_ == mir::Type::Int {
                Ok(())
            } else {
                Err(ValidationError::invalid(format!(
                    "function `{function_name}` has a malformed array length read"
                )))
            }
        }
        mir::RvalueKind::Unary {
            operator: mir::UnaryOperator::BitwiseNot,
            operand: input,
        } => {
            operand(input)?;
            // Complement only reaches the backend on 32- and 64-bit integers:
            // narrower operands are promoted first, and no 8- or 16-bit shape
            // may be emitted.
            if !is_wide_integer(&input.type_) {
                return Err(ValidationError::invalid(format!(
                    "function `{function_name}` has a bitwise complement of unsupported type `{}`",
                    type_name(&input.type_)
                )));
            }
            ensure_same_type(
                &value.type_,
                &input.type_,
                function_name,
                "complement result",
            )
        }
        mir::RvalueKind::ListLength(input)
        | mir::RvalueKind::DictionaryLength(input)
        | mir::RvalueKind::StringByteLength(input)
        | mir::RvalueKind::ListVersion(input)
        | mir::RvalueKind::Cast(input)
        | mir::RvalueKind::Unary { operand: input, .. } => operand(input),
        mir::RvalueKind::MakeInterface { object, .. } => operand(object),
        mir::RvalueKind::Binary {
            left,
            operator,
            right,
        } => {
            operand(left)?;
            operand(right)?;
            ensure_same_type(&left.type_, &right.type_, function_name, "binary operands")?;
            validate_bitwise_operands(*operator, &left.type_, function_name)?;
            let expected = if matches!(
                operator,
                mir::BinaryOperator::Less
                    | mir::BinaryOperator::LessEqual
                    | mir::BinaryOperator::Greater
                    | mir::BinaryOperator::GreaterEqual
                    | mir::BinaryOperator::Equal
                    | mir::BinaryOperator::NotEqual
                    | mir::BinaryOperator::LogicalAnd
                    | mir::BinaryOperator::LogicalOr
            ) {
                &mir::Type::Bool
            } else {
                &left.type_
            };
            ensure_same_type(&value.type_, expected, function_name, "binary result")
        }
        mir::RvalueKind::Equality { left, right, .. } => {
            operand(left)?;
            operand(right)?;
            ensure_same_type(
                &left.type_,
                &right.type_,
                function_name,
                "equality operands",
            )?;
            ensure_same_type(
                &value.type_,
                &mir::Type::Bool,
                function_name,
                "equality result",
            )
        }
    }
}

fn is_wide_integer(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::Int | mir::Type::UInt | mir::Type::Long | mir::Type::ULong
    )
}

/// Fail-closed operand check for the bitwise and shift operators. Shifts are
/// only ever generated for 32- and 64-bit integers, so no 8- or 16-bit shape can
/// reach code generation; bitwise `&`, `|`, `^` also accept `bool`.
fn validate_bitwise_operands(
    operator: mir::BinaryOperator,
    operand_type: &mir::Type,
    function_name: &str,
) -> Result<(), ValidationError> {
    let valid = match operator {
        mir::BinaryOperator::BitwiseAnd
        | mir::BinaryOperator::BitwiseOr
        | mir::BinaryOperator::BitwiseXor => {
            *operand_type == mir::Type::Bool || integer_width_of(operand_type).is_some()
        }
        mir::BinaryOperator::ShiftLeft
        | mir::BinaryOperator::ShiftRight
        | mir::BinaryOperator::UnsignedShiftRight => is_wide_integer(operand_type),
        _ => return Ok(()),
    };
    if valid {
        Ok(())
    } else {
        Err(ValidationError::invalid(format!(
            "function `{function_name}` has a bitwise or shift operation on unsupported type `{}`",
            type_name(operand_type)
        )))
    }
}

fn integer_width_of(type_: &mir::Type) -> Option<u16> {
    match type_ {
        mir::Type::SByte | mir::Type::Byte => Some(8),
        mir::Type::Short | mir::Type::UShort => Some(16),
        mir::Type::Int | mir::Type::UInt => Some(32),
        mir::Type::Long | mir::Type::ULong => Some(64),
        _ => None,
    }
}

fn validate_typed_operand(
    operand: &mir::Operand,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    types: &MirTypeIndex,
) -> Result<(), ValidationError> {
    types.validate_type(&operand.type_, function_name)?;
    match &operand.kind {
        mir::OperandKind::Constant(constant) => {
            validate_constant_type(constant, &operand.type_, function_name)
        }
        mir::OperandKind::Copy(place) => {
            let actual = validate_typed_place(place, function_name, locals, signatures, types)?;
            ensure_same_type(&operand.type_, &actual, function_name, "copied place")
        }
        mir::OperandKind::Function(symbol) => {
            if signatures.contains_key(symbol) {
                Ok(())
            } else {
                Err(ValidationError::invalid(format!(
                    "function `{function_name}` references an unknown function value"
                )))
            }
        }
    }
}

fn validate_typed_place(
    place: &mir::Place,
    function_name: &str,
    locals: &HashMap<mir::LocalId, mir::Type>,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    types: &MirTypeIndex,
) -> Result<mir::Type, ValidationError> {
    match place {
        mir::Place::Local(local) => locals.get(local).cloned().ok_or_else(|| {
            ValidationError::invalid(format!(
                "function `{function_name}` references undeclared local {local:?}"
            ))
        }),
        mir::Place::Field { base, field } => {
            let mir::Type::User(owner) =
                validate_typed_place(base, function_name, locals, signatures, types)?
            else {
                return Err(ValidationError::invalid(format!(
                    "function `{function_name}` reads a struct field from a non-struct place"
                )));
            };
            types
                .struct_fields
                .get(&(owner, *field))
                .cloned()
                .ok_or_else(|| {
                    ValidationError::invalid("MIR struct field does not belong to its base")
                })
        }
        mir::Place::Index {
            array,
            index,
            element_type,
            ..
        } => {
            validate_typed_operand(array, function_name, locals, signatures, types)?;
            validate_typed_operand(index, function_name, locals, signatures, types)?;
            if array.type_ != mir::Type::Array(Box::new(element_type.clone()))
                || index.type_ != mir::Type::Int
            {
                return Err(ValidationError::invalid(format!(
                    "function `{function_name}` contains malformed array index metadata"
                )));
            }
            Ok(element_type.clone())
        }
        mir::Place::ObjectField { object, field } => {
            validate_typed_operand(object, function_name, locals, signatures, types)?;
            let mir::Type::Class(owner) = object.type_ else {
                return Err(ValidationError::invalid(format!(
                    "function `{function_name}` reads an object field from a non-class value"
                )));
            };
            types
                .class_fields
                .get(&(owner, *field))
                .cloned()
                .ok_or_else(|| {
                    ValidationError::invalid("MIR class field does not belong to its object")
                })
        }
        mir::Place::EnumField { base, case, field } => {
            let mir::Type::Enum(owner) =
                validate_typed_place(base, function_name, locals, signatures, types)?
            else {
                return Err(ValidationError::invalid(format!(
                    "function `{function_name}` reads an enum field from a non-enum place"
                )));
            };
            if types.enum_cases.get(case) != Some(&owner) {
                return Err(ValidationError::invalid(
                    "MIR enum case does not belong to its base",
                ));
            }
            types
                .enum_fields
                .get(&(*case, *field))
                .cloned()
                .ok_or_else(|| {
                    ValidationError::invalid("MIR enum field does not belong to its case")
                })
        }
        mir::Place::Symbol(_) => Err(ValidationError::unsupported(
            function_name,
            "module globals, classes, and objects",
        )),
    }
}

fn ensure_same_type(
    actual: &mir::Type,
    expected: &mir::Type,
    function_name: &str,
    operation: &str,
) -> Result<(), ValidationError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ValidationError::invalid(format!(
            "function `{function_name}` has mismatched types for {operation}: `{}` and `{}`",
            type_name(actual),
            type_name(expected)
        )))
    }
}

fn validate_constant_type(
    constant: &mir::Constant,
    type_: &mir::Type,
    function_name: &str,
) -> Result<(), ValidationError> {
    let matches = match constant {
        mir::Constant::Integer(_) => matches!(
            type_,
            mir::Type::SByte
                | mir::Type::Byte
                | mir::Type::Short
                | mir::Type::UShort
                | mir::Type::Int
                | mir::Type::UInt
                | mir::Type::Long
                | mir::Type::ULong
        ),
        mir::Constant::Float(_) => matches!(type_, mir::Type::Float | mir::Type::Double),
        mir::Constant::Decimal(_) => *type_ == mir::Type::Decimal,
        mir::Constant::String(_) => *type_ == mir::Type::String,
        mir::Constant::Character(_) => *type_ == mir::Type::Char,
        mir::Constant::Boolean(_) => *type_ == mir::Type::Bool,
    };
    if matches {
        Ok(())
    } else {
        Err(ValidationError::invalid(format!(
            "function `{function_name}` gives a MIR constant an incompatible type `{}`",
            type_name(type_)
        )))
    }
}
