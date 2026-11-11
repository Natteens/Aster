//! Backend-neutral MIR validation on hand-built modules.

use aster_mir::validate::{ValidationError, validate_types};
use aster_mir::{
    BasicBlock, BasicBlockId, Constant, FieldDefinition, Function, Instruction, Local, LocalId,
    Module, Operand, OperandKind, Place, Rvalue, RvalueKind, StructDefinition, SymbolId,
    Terminator, Type, Visibility,
};

fn local(id: u32, type_: Type) -> Local {
    Local {
        id: LocalId(id),
        symbol: None,
        name: format!("l{id}"),
        type_,
        mutable: true,
        temporary: false,
    }
}

fn constant(type_: Type, constant: Constant) -> Operand {
    Operand {
        type_,
        kind: OperandKind::Constant(constant),
    }
}

fn assign(target: Place, value_type: Type, operand: Operand) -> Instruction {
    Instruction::Assign {
        target,
        value: Rvalue {
            type_: value_type,
            kind: RvalueKind::Use(operand),
        },
    }
}

fn function(symbol: u32, locals: Vec<Local>, instructions: Vec<Instruction>) -> Function {
    Function {
        constructor: false,
        symbol: SymbolId(symbol),
        owner: None,
        name: format!("f{symbol}"),
        visibility: Visibility::Public,
        parameters: Vec::new(),
        locals,
        return_type: Type::Void,
        entry: BasicBlockId(0),
        blocks: vec![BasicBlock {
            id: BasicBlockId(0),
            instructions,
            terminator: Terminator::End,
        }],
        temporary_subregion_candidates: Vec::new(),
    }
}

fn module(functions: Vec<Function>) -> Module {
    Module {
        structs: Vec::new(),
        classes: Vec::new(),
        interfaces: Vec::new(),
        enums: Vec::new(),
        interface_implementations: Vec::new(),
        foreign_functions: Vec::new(),
        functions,
    }
}

fn int(value: &str) -> Operand {
    constant(Type::Int, Constant::Integer(value.to_owned()))
}

fn invalid(result: Result<(), ValidationError>) -> String {
    match result {
        Err(ValidationError::Invalid(message)) => message,
        other => panic!("expected an invalid-MIR error, got {other:?}"),
    }
}

#[test]
fn well_typed_assignment_is_accepted() {
    let valid = function(
        1,
        vec![local(0, Type::Int)],
        vec![assign(Place::Local(LocalId(0)), Type::Int, int("7"))],
    );
    assert_eq!(validate_types(&module(vec![valid])), Ok(()));
}

#[test]
fn assignment_to_a_local_of_another_type_is_rejected() {
    let bad = function(
        1,
        vec![local(0, Type::Bool)],
        vec![assign(Place::Local(LocalId(0)), Type::Int, int("7"))],
    );
    assert!(invalid(validate_types(&module(vec![bad]))).contains("into a place declared `bool`"));
}

#[test]
fn constant_with_an_incompatible_type_is_rejected() {
    let bad = function(
        1,
        vec![local(0, Type::Int)],
        vec![assign(
            Place::Local(LocalId(0)),
            Type::Int,
            constant(Type::Int, Constant::Boolean(true)),
        )],
    );
    assert!(invalid(validate_types(&module(vec![bad]))).contains("incompatible type"));
}

#[test]
fn duplicate_function_symbols_are_rejected() {
    let first = function(1, Vec::new(), Vec::new());
    let second = function(1, Vec::new(), Vec::new());
    assert!(invalid(validate_types(&module(vec![first, second]))).contains("duplicate function"));
}

#[test]
fn reference_to_an_undeclared_class_is_rejected() {
    let bad = function(1, vec![local(0, Type::Class(SymbolId(99)))], Vec::new());
    assert!(invalid(validate_types(&module(vec![bad]))).contains("nominal type"));
}

#[test]
fn field_symbol_shared_between_two_structs_is_rejected() {
    let field = FieldDefinition {
        symbol: SymbolId(10),
        name: "x".to_owned(),
        type_: Type::Int,
    };
    let mut shared = module(Vec::new());
    for symbol in [20, 21] {
        shared.structs.push(StructDefinition {
            symbol: SymbolId(symbol),
            name: format!("S{symbol}"),
            fields: vec![field.clone()],
        });
    }
    assert!(invalid(validate_types(&shared)).contains("duplicate field symbol"));
}

#[test]
fn global_symbol_places_are_reported_as_unsupported_not_invalid() {
    let bad = function(
        1,
        Vec::new(),
        vec![assign(Place::Symbol(SymbolId(5)), Type::Int, int("1"))],
    );
    assert!(matches!(
        validate_types(&module(vec![bad])),
        Err(ValidationError::Unsupported { .. })
    ));
}
