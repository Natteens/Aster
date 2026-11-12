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
            vtable: Vec::new(),
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

    fn class_test(from: u32, target: u32, result: Type) -> Vec<Instruction> {
        vec![Instruction::Assign {
            target: Place::Local(LocalId(2)),
            value: Rvalue {
                type_: result,
                kind: RvalueKind::ClassTest {
                    object: Operand {
                        type_: Type::Class(SymbolId(from)),
                        kind: OperandKind::Copy(Place::Local(LocalId(0))),
                    },
                    class: SymbolId(target),
                },
            },
        }]
    }

    fn with_test_local(
        classes: Vec<ClassDefinition>,
        from: u32,
        instructions: Vec<Instruction>,
    ) -> Module {
        let mut m = with(classes, instructions);
        m.functions[0].locals[0] = local(0, Type::Class(SymbolId(from)));
        m.functions[0].locals.push(local(2, Type::Bool));
        m
    }

    #[test]
    fn class_test_requires_related_classes_and_a_bool_result() {
        let classes = vec![class(10, None), class(11, Some(10)), class(12, None)];
        // Downcast direction, upcast direction and the same class all can match.
        for (from, target) in [(10, 11), (11, 10), (11, 11)] {
            assert!(
                validate_types(&with_test_local(
                    classes.clone(),
                    from,
                    class_test(from, target, Type::Bool)
                ))
                .is_ok(),
                "{from} tested against {target}"
            );
        }
        // An unrelated class can never match.
        assert!(
            validate_types(&with_test_local(
                classes.clone(),
                11,
                class_test(11, 12, Type::Bool)
            ))
            .is_err()
        );
        // The result must be `bool`.
        assert!(
            validate_types(&with_test_local(classes, 10, class_test(10, 11, Type::Int))).is_err()
        );
    }

    #[test]
    fn downcast_is_accepted_between_related_classes_only() {
        let classes = vec![class(10, None), class(11, Some(10)), class(12, None)];
        // Local 0 is `Class(11)`, local 1 is `Class(10)`.
        let narrow = |operand_class: u32, result_class: u32, from: u32, to: u32| {
            vec![Instruction::Assign {
                target: Place::Local(LocalId(to)),
                value: Rvalue {
                    type_: Type::Class(SymbolId(result_class)),
                    kind: RvalueKind::Downcast(Operand {
                        type_: Type::Class(SymbolId(operand_class)),
                        kind: OperandKind::Copy(Place::Local(LocalId(from))),
                    }),
                },
            }]
        };
        assert!(validate_types(&with(classes.clone(), narrow(10, 11, 1, 0))).is_ok());
        assert!(validate_types(&with(classes.clone(), narrow(11, 10, 0, 1))).is_ok());
        assert!(validate_types(&with(classes, narrow(10, 12, 1, 0))).is_err());
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

/// Object model B1, pull request 2: virtual tables and virtual calls.
mod virtual_tables {
    use super::*;
    use aster_mir::{ClassDefinition, VirtualSlot};

    const BASE: u32 = 10;
    const DERIVED: u32 = 11;
    /// The method that introduces the slot (`BASE.Go(int) -> int`).
    const ROOT: u32 = 20;
    /// `DERIVED.Go(int) -> int`, the override.
    const OVERRIDE: u32 = 21;

    fn class(symbol: u32, base: Option<u32>, vtable: Vec<VirtualSlot>) -> ClassDefinition {
        ClassDefinition {
            symbol: SymbolId(symbol),
            name: format!("C{symbol}"),
            base: base.map(SymbolId),
            is_abstract: false,
            is_sealed: false,
            vtable,
            fields: Vec::new(),
        }
    }

    fn slot(method: u32, implementation: Option<u32>) -> VirtualSlot {
        VirtualSlot {
            method: SymbolId(method),
            name: "Go".to_owned(),
            parameters: vec![Type::Int],
            return_type: Type::Int,
            implementation: implementation.map(SymbolId),
        }
    }

    /// `owner.method(this, int) -> int`.
    fn method(symbol: u32, owner: u32) -> Function {
        let mut f = function(symbol, Vec::new(), Vec::new());
        f.owner = Some(SymbolId(owner));
        f.parameters = vec![local(0, Type::Class(SymbolId(owner))), local(1, Type::Int)];
        f.return_type = Type::Int;
        f.blocks[0].terminator = Terminator::Return(Some(int("0")));
        f
    }

    fn module_of(classes: Vec<ClassDefinition>, extra: Vec<Function>) -> Module {
        let mut m = module(extra);
        m.classes = classes;
        m
    }

    /// Base introduces the slot, derived overrides it.
    fn valid() -> Module {
        module_of(
            vec![
                class(BASE, None, vec![slot(ROOT, Some(ROOT))]),
                class(DERIVED, Some(BASE), vec![slot(ROOT, Some(OVERRIDE))]),
            ],
            vec![method(ROOT, BASE), method(OVERRIDE, DERIVED)],
        )
    }

    fn message(result: Result<(), ValidationError>) -> String {
        match result {
            Err(ValidationError::Invalid(message)) => message,
            other => panic!("expected an invalid-MIR error, found {other:?}"),
        }
    }

    #[test]
    fn a_consistent_override_chain_is_accepted() {
        assert!(validate_types(&valid()).is_ok());
    }

    #[test]
    fn a_table_shorter_than_the_base_or_with_a_changed_slot_signature_is_rejected() {
        let mut shorter = valid();
        shorter.classes[1].vtable.clear();
        assert!(message(validate_types(&shorter)).contains("shorter than its base"));

        let mut other_signature = valid();
        other_signature.classes[1].vtable[0].parameters = vec![Type::Long];
        assert!(
            message(validate_types(&other_signature))
                .contains("does not match the slot it inherits")
        );

        let mut other_method = valid();
        other_method.classes[1].vtable[0].method = SymbolId(99);
        assert!(
            message(validate_types(&other_method)).contains("does not match the slot it inherits")
        );
    }

    #[test]
    fn an_override_must_be_declared_by_the_class_with_the_exact_signature() {
        // The override is not declared by the derived class.
        let mut wrong_owner = valid();
        wrong_owner.functions[1].owner = Some(SymbolId(BASE));
        wrong_owner.functions[1].parameters[0].type_ = Type::Class(SymbolId(BASE));
        assert!(
            message(validate_types(&wrong_owner)).contains("does not match the slot signature")
        );

        // The implementation has another return type.
        let mut wrong_return = valid();
        wrong_return.functions[1].return_type = Type::Long;
        wrong_return.functions[1].blocks[0].terminator = Terminator::Return(Some(Operand {
            type_: Type::Long,
            kind: OperandKind::Constant(Constant::Integer("0".to_owned())),
        }));
        assert!(
            message(validate_types(&wrong_return)).contains("does not match the slot signature")
        );

        // Unknown implementation.
        let mut unknown = valid();
        unknown.classes[1].vtable[0].implementation = Some(SymbolId(404));
        assert!(message(validate_types(&unknown)).contains("unknown implementation"));
    }

    #[test]
    fn abstract_slots_belong_to_abstract_classes_and_cannot_be_re_abstracted() {
        // A concrete class with an abstract slot.
        let mut concrete = module_of(vec![class(BASE, None, vec![slot(ROOT, None)])], Vec::new());
        assert!(message(validate_types(&concrete)).contains("abstract virtual slot"));
        // The same slot in an abstract class is fine.
        concrete.classes[0].is_abstract = true;
        assert!(validate_types(&concrete).is_ok());

        // Implemented in the base, abstract again in the derived class.
        let mut re_abstract = valid();
        re_abstract.classes[1].vtable[0].implementation = None;
        re_abstract.classes[1].is_abstract = true;
        assert!(message(validate_types(&re_abstract)).contains("re-abstracts"));
    }

    #[test]
    fn an_abstract_slot_may_be_implemented_by_a_derived_class() {
        let mut m = module_of(
            vec![
                class(BASE, None, vec![slot(ROOT, None)]),
                class(DERIVED, Some(BASE), vec![slot(ROOT, Some(OVERRIDE))]),
            ],
            vec![method(OVERRIDE, DERIVED)],
        );
        m.classes[0].is_abstract = true;
        assert!(validate_types(&m).is_ok());
    }

    #[test]
    fn a_method_cannot_own_two_slots() {
        let mut m = module_of(
            vec![class(
                BASE,
                None,
                vec![slot(ROOT, Some(ROOT)), slot(ROOT, Some(ROOT))],
            )],
            vec![method(ROOT, BASE)],
        );
        assert!(message(validate_types(&m)).contains("lists method"));
        m.classes[0].vtable.pop();
        assert!(validate_types(&m).is_ok());
    }

    fn call(receiver: Type, slot_index: u32, method: u32, argument: Type, result: Type) -> Module {
        let mut m = valid();
        let mut caller = function(
            1,
            vec![local(0, receiver.clone()), local(1, result.clone())],
            vec![Instruction::CallVirtual {
                destination: Some(Place::Local(LocalId(1))),
                receiver: Operand {
                    type_: receiver,
                    kind: OperandKind::Copy(Place::Local(LocalId(0))),
                },
                method: SymbolId(method),
                slot: slot_index,
                arguments: vec![Operand {
                    type_: argument,
                    kind: OperandKind::Constant(Constant::Integer("1".to_owned())),
                }],
                return_type: result,
            }],
        );
        caller.parameters = vec![caller.locals.remove(0)];
        m.functions.push(caller);
        m
    }

    #[test]
    fn v8_a_virtual_call_is_checked_against_the_receiver_class_table() {
        let ok = call(
            Type::Class(SymbolId(DERIVED)),
            0,
            ROOT,
            Type::Int,
            Type::Int,
        );
        assert!(validate_types(&ok).is_ok());

        // V8: the slot index is not below the vtable length.
        let outside = call(
            Type::Class(SymbolId(DERIVED)),
            1,
            ROOT,
            Type::Int,
            Type::Int,
        );
        assert!(message(validate_types(&outside)).contains("outside the virtual table"));

        let wrong_method = call(Type::Class(SymbolId(DERIVED)), 0, 77, Type::Int, Type::Int);
        assert!(message(validate_types(&wrong_method)).contains("does not own the slot"));

        let wrong_result = call(
            Type::Class(SymbolId(DERIVED)),
            0,
            ROOT,
            Type::Int,
            Type::Long,
        );
        assert!(message(validate_types(&wrong_result)).contains("return type"));

        let wrong_argument = call(
            Type::Class(SymbolId(DERIVED)),
            0,
            ROOT,
            Type::Long,
            Type::Int,
        );
        assert!(message(validate_types(&wrong_argument)).contains("slot signature"));

        let not_a_class = call(Type::Int, 0, ROOT, Type::Int, Type::Int);
        assert!(message(validate_types(&not_a_class)).contains("not a class"));
    }
}

/// Object model B1, pull requests 3 and 4: dynamic interface tests and
/// lookups, and the narrowing of an interface value back to a class.
mod interface_search {
    use super::*;
    use aster_mir::{ClassDefinition, InterfaceDefinition};

    fn class(symbol: u32) -> ClassDefinition {
        ClassDefinition {
            symbol: SymbolId(symbol),
            name: format!("C{symbol}"),
            base: None,
            is_abstract: false,
            is_sealed: false,
            vtable: Vec::new(),
            fields: Vec::new(),
        }
    }

    /// Local 0 is `object_type`, local 1 is `result_type`; the module has class
    /// 10 and interface 20.
    fn with(object_type: Type, result_type: Type, kind: RvalueKind) -> Module {
        let mut m = module(vec![function(
            1,
            vec![local(0, object_type), local(1, result_type.clone())],
            vec![Instruction::Assign {
                target: Place::Local(LocalId(1)),
                value: Rvalue {
                    type_: result_type,
                    kind,
                },
            }],
        )]);
        m.classes = vec![class(10)];
        m.interfaces = vec![InterfaceDefinition {
            symbol: SymbolId(20),
            name: "I".to_owned(),
            methods: Vec::new(),
        }];
        m
    }

    fn object(type_: Type) -> Operand {
        Operand {
            type_,
            kind: OperandKind::Copy(Place::Local(LocalId(0))),
        }
    }

    #[test]
    fn interface_test_and_lookup_accept_a_class_or_an_interface_object() {
        for object_type in [Type::Class(SymbolId(10)), Type::Interface(SymbolId(20))] {
            let test = RvalueKind::InterfaceTest {
                object: object(object_type.clone()),
                interface: SymbolId(20),
            };
            assert!(validate_types(&with(object_type.clone(), Type::Bool, test)).is_ok());
            let lookup = RvalueKind::InterfaceLookup {
                object: object(object_type.clone()),
                interface: SymbolId(20),
            };
            assert!(
                validate_types(&with(object_type, Type::Interface(SymbolId(20)), lookup)).is_ok()
            );
        }
    }

    #[test]
    fn interface_test_and_lookup_reject_malformed_shapes() {
        let class_type = Type::Class(SymbolId(10));
        let test = |object_type: Type, interface: u32, result: Type| {
            let kind = RvalueKind::InterfaceTest {
                object: object(object_type.clone()),
                interface: SymbolId(interface),
            };
            validate_types(&with(object_type, result, kind))
        };
        // The result must be `bool`.
        assert!(test(class_type.clone(), 20, Type::Int).is_err());
        // The tested name must be an interface, not a class or an unknown symbol.
        assert!(test(class_type.clone(), 10, Type::Bool).is_err());
        assert!(test(class_type, 99, Type::Bool).is_err());
        // Only a class reference or an interface value has an object to inspect.
        assert!(test(Type::Int, 20, Type::Bool).is_err());

        let lookup = |object_type: Type, interface: u32, result: Type| {
            let kind = RvalueKind::InterfaceLookup {
                object: object(object_type.clone()),
                interface: SymbolId(interface),
            };
            validate_types(&with(object_type, result, kind))
        };
        // The result must be the looked-up interface.
        assert!(lookup(Type::Class(SymbolId(10)), 20, Type::Bool).is_err());
        assert!(lookup(Type::Class(SymbolId(10)), 99, Type::Interface(SymbolId(99))).is_err());
        assert!(lookup(Type::Int, 20, Type::Interface(SymbolId(20))).is_err());
    }

    #[test]
    fn an_interface_value_narrows_to_a_class_and_a_class_test_accepts_an_interface_subject() {
        let narrow = RvalueKind::Downcast(object(Type::Interface(SymbolId(20))));
        assert!(
            validate_types(&with(
                Type::Interface(SymbolId(20)),
                Type::Class(SymbolId(10)),
                narrow
            ))
            .is_ok()
        );
        let test = RvalueKind::ClassTest {
            object: object(Type::Interface(SymbolId(20))),
            class: SymbolId(10),
        };
        assert!(validate_types(&with(Type::Interface(SymbolId(20)), Type::Bool, test)).is_ok());
        // A class test names a class, never an interface.
        let wrong = RvalueKind::ClassTest {
            object: object(Type::Interface(SymbolId(20))),
            class: SymbolId(20),
        };
        assert!(validate_types(&with(Type::Interface(SymbolId(20)), Type::Bool, wrong)).is_err());
    }
}
