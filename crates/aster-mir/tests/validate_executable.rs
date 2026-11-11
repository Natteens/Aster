//! Invalid MIR rejected by the backend-neutral executability checks that
//! moved from the Cranelift backend (T-ARCH-2c).

use std::collections::{HashMap, HashSet};

use aster_mir::validate::ValidationError;
use aster_mir::validate::executable::{
    validate_block_target, validate_enum_construct_shapes, validate_no_console_io_in_workers,
    validate_struct_literal_shapes,
};
use aster_mir::{
    BasicBlock, BasicBlockId, Constant, Function, Instruction, Intrinsic, Local, LocalId, Module,
    Operand, OperandKind, Place, Rvalue, RvalueKind, SymbolId, Terminator, Type, Visibility,
};

fn function(
    symbol: u32,
    name: &str,
    locals: Vec<Local>,
    instructions: Vec<Instruction>,
) -> Function {
    Function {
        constructor: false,
        symbol: SymbolId(symbol),
        owner: None,
        name: name.to_owned(),
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

fn assign_rvalue(type_: Type, kind: RvalueKind) -> Instruction {
    Instruction::Assign {
        target: Place::Local(LocalId(0)),
        value: Rvalue { type_, kind },
    }
}

fn message(result: Result<(), ValidationError>) -> String {
    match result {
        Err(ValidationError::Invalid(message)) => message,
        other => panic!("expected invalid MIR, got {other:?}"),
    }
}

#[test]
fn a_jump_to_an_unknown_block_is_rejected() {
    let blocks = HashSet::from([BasicBlockId(0)]);
    assert_eq!(validate_block_target(BasicBlockId(0), "f", &blocks), Ok(()));
    assert!(
        message(validate_block_target(BasicBlockId(9), "f", &blocks))
            .contains("unknown basic block")
    );
}

#[test]
fn an_enum_construction_of_an_undeclared_enum_is_rejected() {
    let enum_type = Type::Enum(SymbolId(50));
    let bad = function(
        1,
        "f",
        vec![local(0, enum_type.clone())],
        vec![assign_rvalue(
            enum_type,
            RvalueKind::EnumConstruct {
                case: SymbolId(51),
                tag: 0,
                fields: Vec::new(),
            },
        )],
    );
    assert!(
        message(validate_enum_construct_shapes(&bad, &HashMap::new()))
            .contains("unknown enum type")
    );
}

#[test]
fn an_aggregate_of_an_undeclared_struct_is_rejected() {
    let struct_type = Type::User(SymbolId(60));
    let bad = function(
        1,
        "f",
        vec![local(0, struct_type.clone())],
        vec![assign_rvalue(
            struct_type,
            RvalueKind::Aggregate(Vec::new()),
        )],
    );
    assert!(
        message(validate_struct_literal_shapes(&bad, &HashMap::new()))
            .contains("unknown struct type")
    );
}

#[test]
fn a_worker_body_that_reaches_console_output_is_a_policy_rejection() {
    let write = Instruction::CallIntrinsic {
        destination: None,
        intrinsic: Intrinsic::ConsoleWriteLine,
        arguments: vec![Operand {
            type_: Type::String,
            kind: OperandKind::Constant(Constant::String("x".to_owned())),
        }],
        return_type: Type::Void,
    };
    let helper = function(2, "Helper", Vec::new(), vec![write]);
    let worker = function(
        3,
        "Work",
        Vec::new(),
        vec![Instruction::Call {
            destination: None,
            function: SymbolId(2),
            arguments: Vec::new(),
            return_type: Type::Void,
        }],
    );
    let spawn = Instruction::CallIntrinsic {
        destination: None,
        intrinsic: Intrinsic::TaskRun,
        arguments: vec![Operand {
            type_: Type::Int,
            kind: OperandKind::Function(SymbolId(3)),
        }],
        return_type: Type::Task(Box::new(Type::Int)),
    };
    let main = function(1, "Main", Vec::new(), vec![spawn]);
    let module = Module {
        structs: Vec::new(),
        classes: Vec::new(),
        interfaces: Vec::new(),
        enums: Vec::new(),
        interface_implementations: Vec::new(),
        foreign_functions: Vec::new(),
        functions: vec![main, worker, helper],
    };
    let Err(ValidationError::Rejected(error)) = validate_no_console_io_in_workers(&module) else {
        panic!("a worker host operation is a policy rejection, not invalid MIR");
    };
    assert!(error.contains("`Task.Run` with a worker body"), "{error}");

    let mut safe = module;
    safe.functions[2].blocks[0].instructions.clear();
    assert_eq!(validate_no_console_io_in_workers(&safe), Ok(()));
}

mod phased {
    use aster_mir::validate::ValidationError;
    use aster_mir::validate::executable::{Phase, validate_cfg, validate_module};
    use aster_mir::{
        BasicBlock, BasicBlockId, Instruction, Module, OwnedRegionId, TemporarySubregionId,
        Terminator,
    };

    fn module_with(instructions: Vec<Instruction>) -> Module {
        Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: vec![super::function(1, "f", Vec::new(), instructions)],
        }
    }

    fn is_invalid(result: &Result<(), ValidationError>) -> bool {
        matches!(result, Err(ValidationError::Invalid(_)))
    }

    /// Region violations are reported as unsupported (the backend's wording);
    /// the compiler's per-pass check treats them as defects.
    fn is_violation(result: &Result<(), ValidationError>) -> bool {
        matches!(
            result,
            Err(ValidationError::Invalid(_) | ValidationError::Unsupported { .. })
        )
    }

    #[test]
    fn cfg_rejects_duplicate_blocks_missing_entry_and_unknown_branch_targets() {
        let mut duplicate = super::function(1, "f", Vec::new(), Vec::new());
        duplicate.blocks.push(duplicate.blocks[0].clone());
        assert!(is_invalid(&validate_cfg(&duplicate)));

        let mut missing_entry = super::function(1, "f", Vec::new(), Vec::new());
        missing_entry.entry = BasicBlockId(4);
        assert!(is_invalid(&validate_cfg(&missing_entry)));

        let mut branch = super::function(1, "f", Vec::new(), Vec::new());
        branch.blocks[0].terminator = Terminator::Goto(BasicBlockId(1));
        branch.blocks.push(BasicBlock {
            id: BasicBlockId(1),
            instructions: Vec::new(),
            terminator: Terminator::Goto(BasicBlockId(7)),
        });
        assert!(is_invalid(&validate_cfg(&branch)));
    }

    #[test]
    fn owned_region_checks_apply_only_after_owned_region_lowering() {
        let unbalanced = module_with(vec![Instruction::OwnedRegionExit {
            id: OwnedRegionId(0),
            invalidated: Vec::new(),
        }]);
        assert_eq!(validate_module(&unbalanced, Phase::Lowered), Ok(()));
        assert_eq!(
            validate_module(&unbalanced, Phase::TemporarySubregions),
            Ok(())
        );
        assert!(is_violation(&validate_module(&unbalanced, Phase::Final)));
    }

    #[test]
    fn subregion_checks_apply_from_the_aarm_phase_on() {
        let unbalanced = module_with(vec![Instruction::TemporarySubregionExit {
            id: TemporarySubregionId(0),
        }]);
        assert_eq!(validate_module(&unbalanced, Phase::Lowered), Ok(()));
        assert!(is_violation(&validate_module(
            &unbalanced,
            Phase::TemporarySubregions
        )));
        assert!(is_violation(&validate_module(&unbalanced, Phase::Final)));
    }
}
