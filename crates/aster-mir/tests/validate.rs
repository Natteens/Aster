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

mod hierarchy {
    use super::*;
    use aster_mir::ClassDefinition;

    fn class(symbol: u32, base: Option<u32>) -> ClassDefinition {
        ClassDefinition {
            symbol: SymbolId(symbol),
            name: format!("C{symbol}"),
            base: base.map(SymbolId),
            is_abstract: false,
            is_sealed: false,
            fields: Vec::new(),
        }
    }

    fn with(classes: Vec<ClassDefinition>, instructions: Vec<Instruction>) -> Module {
        let mut m = module(vec![function(
            1,
            vec![
                local(0, Type::Class(SymbolId(11))),
                local(1, Type::Class(SymbolId(10))),
            ],
            instructions,
        )]);
        m.classes = classes;
        m
    }

    fn upcast(from: u32, to: u32) -> Vec<Instruction> {
        vec![Instruction::Assign {
            target: Place::Local(LocalId(1)),
            value: Rvalue {
                type_: Type::Class(SymbolId(to)),
                kind: RvalueKind::Upcast(Operand {
                    type_: Type::Class(SymbolId(from)),
                    kind: OperandKind::Copy(Place::Local(LocalId(0))),
                }),
            },
        }]
    }

    #[test]
    fn upcast_along_the_base_chain_is_accepted_and_the_reverse_is_rejected() {
        let classes = vec![class(10, None), class(11, Some(10))];
        assert!(validate_types(&with(classes.clone(), upcast(11, 10))).is_ok());
        let reverse = vec![Instruction::Assign {
            target: Place::Local(LocalId(0)),
            value: Rvalue {
                type_: Type::Class(SymbolId(11)),
                kind: RvalueKind::Upcast(Operand {
                    type_: Type::Class(SymbolId(10)),
                    kind: OperandKind::Copy(Place::Local(LocalId(1))),
                }),
            },
        }];
        assert!(validate_types(&with(classes, reverse)).is_err());
    }

    #[test]
    fn cycles_sealed_bases_and_abstract_allocation_are_rejected() {
        let cyclic = vec![class(10, Some(11)), class(11, Some(10))];
        assert!(validate_types(&with(cyclic, Vec::new())).is_err());
        let mut sealed = vec![class(10, None), class(11, Some(10))];
        sealed[0].is_sealed = true;
        assert!(validate_types(&with(sealed, Vec::new())).is_err());
        let mut abstract_base = vec![class(10, None), class(11, Some(10))];
        abstract_base[0].is_abstract = true;
        let allocate = vec![Instruction::AllocateObject {
            destination: Place::Local(LocalId(1)),
            class: SymbolId(10),
            region: aster_mir::AllocationRegion::Persistent,
        }];
        assert!(validate_types(&with(abstract_base, allocate)).is_err());
    }

    #[test]
    fn a_base_that_is_not_a_class_or_a_shared_field_symbol_is_rejected() {
        assert!(validate_types(&with(vec![class(11, Some(99))], Vec::new())).is_err());
        let mut classes = vec![class(10, None), class(11, Some(10))];
        let field = FieldDefinition {
            symbol: SymbolId(50),
            name: "x".to_owned(),
            type_: Type::Int,
        };
        classes[0].fields.push(field.clone());
        classes[1].fields.push(field);
        assert!(validate_types(&with(classes, Vec::new())).is_err());
    }
}
