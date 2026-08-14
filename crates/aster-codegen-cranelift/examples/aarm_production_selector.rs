//! Release-only application-style evidence for the experimental AARM production selector.
//!
//! The four modes execute identical backend-neutral MIR: ordinary function lifetime,
//! every safe AARM candidate, the production hidden-backing-growth selector, and the
//! candidate v2 structural array extension. JIT preparation occurs before warmup and
//! timed execution. This is a manual research harness; timings are informational and
//! never CI assertions.

use std::time::Instant;

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, PreparedSequentialExecution};
use aster_compiler::{
    AarmTemporarySubregionProfitabilityPolicy, lower_aarm_temporary_subregions_for_research,
    lower_aarm_temporary_subregions_with_policy_for_research,
};
use aster_mir as mir;

const RUN: mir::SymbolId = mir::SymbolId(1);
const BOX: mir::SymbolId = mir::SymbolId(10);
const FIELD: mir::SymbolId = mir::SymbolId(11);
const BUILDER: mir::SymbolId = mir::SymbolId(12);
const SAMPLES: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Baseline,
    Raw,
    ProductionV1,
    ProductionV2,
}

impl Mode {
    const fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Raw => "raw",
            Self::ProductionV1 => "production-v1",
            Self::ProductionV2 => "production-v2",
        }
    }
}

struct Workload {
    name: &'static str,
    module: mir::Module,
    expected: ExecutionValue,
}

struct Measurement {
    median_ms: f64,
    stats: MemoryStats,
    static_regions: usize,
    dynamic_regions: u64,
    lowering_micros: u128,
}

fn iteration_scale() -> usize {
    std::env::var("ASTER_AARM_SELECTOR_ITERATIONS").map_or(100_000, |value| {
        value
            .parse()
            .expect("ASTER_AARM_SELECTOR_ITERATIONS must be a positive integer")
    })
}

fn workload_selected(name: &str) -> bool {
    std::env::var("ASTER_AARM_SELECTOR_WORKLOADS").map_or(true, |value| {
        value
            .split(',')
            .map(str::trim)
            .any(|requested| requested == name)
    })
}

fn local(id: u32, name: &str, type_: mir::Type) -> mir::Local {
    mir::Local {
        id: mir::LocalId(id),
        symbol: None,
        name: name.to_owned(),
        type_,
        mutable: true,
        temporary: true,
    }
}

fn integer(value: i32) -> mir::Operand {
    mir::Operand {
        type_: mir::Type::Int,
        kind: mir::OperandKind::Constant(mir::Constant::Integer(value.to_string())),
    }
}

fn long(value: i64) -> mir::Operand {
    mir::Operand {
        type_: mir::Type::Long,
        kind: mir::OperandKind::Constant(mir::Constant::Integer(value.to_string())),
    }
}

fn boolean(value: bool) -> mir::Operand {
    mir::Operand {
        type_: mir::Type::Bool,
        kind: mir::OperandKind::Constant(mir::Constant::Boolean(value)),
    }
}

fn string(value: &str) -> mir::Operand {
    mir::Operand {
        type_: mir::Type::String,
        kind: mir::OperandKind::Constant(mir::Constant::String(value.to_owned())),
    }
}

fn copy(local: u32, type_: mir::Type) -> mir::Operand {
    mir::Operand {
        type_,
        kind: mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(local))),
    }
}

fn use_value(target: u32, value: mir::Operand) -> mir::Instruction {
    mir::Instruction::Assign {
        target: mir::Place::Local(mir::LocalId(target)),
        value: mir::Rvalue {
            type_: value.type_.clone(),
            kind: mir::RvalueKind::Use(value),
        },
    }
}

fn add_constant(target: u32, value: i32) -> mir::Instruction {
    mir::Instruction::Assign {
        target: mir::Place::Local(mir::LocalId(target)),
        value: mir::Rvalue {
            type_: mir::Type::Int,
            kind: mir::RvalueKind::Binary {
                left: copy(target, mir::Type::Int),
                operator: mir::BinaryOperator::Add,
                right: integer(value),
            },
        },
    }
}

fn less(index: u32, condition: u32, iterations: usize) -> mir::Instruction {
    mir::Instruction::Assign {
        target: mir::Place::Local(mir::LocalId(condition)),
        value: mir::Rvalue {
            type_: mir::Type::Bool,
            kind: mir::RvalueKind::Binary {
                left: copy(index, mir::Type::Int),
                operator: mir::BinaryOperator::Less,
                right: integer(i32::try_from(iterations).expect("iterations fit int")),
            },
        },
    }
}

fn toggle(local: u32) -> mir::Instruction {
    mir::Instruction::Assign {
        target: mir::Place::Local(mir::LocalId(local)),
        value: mir::Rvalue {
            type_: mir::Type::Bool,
            kind: mir::RvalueKind::Unary {
                operator: mir::UnaryOperator::Not,
                operand: copy(local, mir::Type::Bool),
            },
        },
    }
}

fn temporary_object(destination: u32) -> mir::Instruction {
    mir::Instruction::AllocateObject {
        destination: mir::Place::Local(mir::LocalId(destination)),
        class: BOX,
        region: mir::AllocationRegion::Temporary,
    }
}

fn observe_object(object: u32, result: u32) -> mir::Instruction {
    let object = copy(object, mir::Type::Class(BOX));
    mir::Instruction::Assign {
        target: mir::Place::Local(mir::LocalId(result)),
        value: mir::Rvalue {
            type_: mir::Type::Bool,
            kind: mir::RvalueKind::Equality {
                left: object.clone(),
                right: object,
                negated: false,
            },
        },
    }
}

fn temporary_array(destination: u32, length: i32) -> mir::Instruction {
    mir::Instruction::AllocateArray {
        destination: mir::Place::Local(mir::LocalId(destination)),
        element_type: mir::Type::Int,
        length: integer(length),
        requires_default: true,
        region: mir::AllocationRegion::Temporary,
    }
}

fn temporary_array_with_length(destination: u32, length: u32) -> mir::Instruction {
    mir::Instruction::AllocateArray {
        destination: mir::Place::Local(mir::LocalId(destination)),
        element_type: mir::Type::Int,
        length: copy(length, mir::Type::Int),
        requires_default: true,
        region: mir::AllocationRegion::Temporary,
    }
}

fn temporary_string(destination: u32) -> mir::Instruction {
    mir::Instruction::CallIntrinsic {
        destination: Some(mir::Place::Local(mir::LocalId(destination))),
        intrinsic: mir::Intrinsic::StringFromLongTemporary,
        arguments: vec![long(42)],
        return_type: mir::Type::String,
    }
}

fn temporary_substring(destination: u32) -> mir::Instruction {
    mir::Instruction::CallIntrinsic {
        destination: Some(mir::Place::Local(mir::LocalId(destination))),
        intrinsic: mir::Intrinsic::StringSubstringRangeTemporary,
        arguments: vec![string("application"), integer(1), integer(5)],
        return_type: mir::Type::String,
    }
}

fn temporary_join(destination: u32) -> mir::Instruction {
    mir::Instruction::CallIntrinsic {
        destination: Some(mir::Place::Local(mir::LocalId(destination))),
        intrinsic: mir::Intrinsic::StringJoinTemporary,
        arguments: vec![string("a"), string("b"), string("c"), string("d")],
        return_type: mir::Type::String,
    }
}

fn temporary_large_substring(destination: u32, length: i32) -> mir::Instruction {
    mir::Instruction::CallIntrinsic {
        destination: Some(mir::Place::Local(mir::LocalId(destination))),
        intrinsic: mir::Intrinsic::StringSubstringRangeTemporary,
        arguments: vec![
            string(&"x".repeat(usize::try_from(length + 1).expect("positive string length"))),
            integer(0),
            integer(length),
        ],
        return_type: mir::Type::String,
    }
}

fn temporary_builder(destination: u32) -> mir::Instruction {
    mir::Instruction::AllocateStringBuilder {
        destination: mir::Place::Local(mir::LocalId(destination)),
        class: BUILDER,
        region: mir::AllocationRegion::Temporary,
    }
}

fn builder_append(builder: u32, value: mir::Operand) -> mir::Instruction {
    mir::Instruction::StringBuilderAppend {
        builder: copy(builder, mir::Type::Class(BUILDER)),
        value,
        class: BUILDER,
    }
}

fn builder_snapshot(builder: u32, destination: u32) -> mir::Instruction {
    mir::Instruction::StringBuilderToString {
        destination: mir::Place::Local(mir::LocalId(destination)),
        builder: copy(builder, mir::Type::Class(BUILDER)),
        class: BUILDER,
        region: mir::AllocationRegion::Temporary,
    }
}

fn temporary_list(destination: u32) -> mir::Instruction {
    mir::Instruction::AllocateList {
        destination: mir::Place::Local(mir::LocalId(destination)),
        element_type: mir::Type::Int,
        region: mir::AllocationRegion::Temporary,
    }
}

fn list_add(list: u32, value: i32) -> mir::Instruction {
    mir::Instruction::ListAdd {
        list: copy(list, mir::Type::List(Box::new(mir::Type::Int))),
        value: integer(value),
    }
}

fn temporary_dictionary(destination: u32) -> mir::Instruction {
    mir::Instruction::AllocateDictionary {
        destination: mir::Place::Local(mir::LocalId(destination)),
        key_type: mir::Type::Int,
        value_type: mir::Type::Int,
        region: mir::AllocationRegion::Temporary,
    }
}

fn dictionary_set(dictionary: u32, destination: u32, key: i32) -> mir::Instruction {
    mir::Instruction::DictionarySet {
        destination: mir::Place::Local(mir::LocalId(destination)),
        dictionary: copy(
            dictionary,
            mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int)),
        ),
        key: integer(key),
        value: integer(key * 3 + 1),
    }
}

fn module(function: mir::Function) -> mir::Module {
    mir::Module {
        structs: Vec::new(),
        classes: vec![
            mir::ClassDefinition {
                symbol: BOX,
                name: "Box".to_owned(),
                fields: vec![mir::FieldDefinition {
                    symbol: FIELD,
                    name: "value".to_owned(),
                    type_: mir::Type::Int,
                }],
            },
            mir::ClassDefinition {
                symbol: BUILDER,
                name: "aster.core::StringBuilder".to_owned(),
                fields: Vec::new(),
            },
        ],
        interfaces: Vec::new(),
        enums: Vec::new(),
        interface_implementations: Vec::new(),
        functions: vec![function],
    }
}

fn branched_loop_module(
    iterations: usize,
    scalar_operations: usize,
    mut locals: Vec<mir::Local>,
    mut allocation_work: Vec<mir::Instruction>,
) -> mir::Module {
    // Locals 0..=4 are fixed loop state. Workload-specific locals start at 5.
    let mut body = Vec::new();
    body.append(&mut allocation_work);
    body.extend((0..scalar_operations).map(|_| add_constant(2, 1)));
    locals.splice(
        0..0,
        [
            local(0, "index", mir::Type::Int),
            local(1, "condition", mir::Type::Bool),
            local(2, "checksum", mir::Type::Int),
            local(3, "alternate", mir::Type::Bool),
            local(4, "observed", mir::Type::Bool),
        ],
    );
    module(mir::Function {
        constructor: false,
        symbol: RUN,
        owner: None,
        name: "Run".to_owned(),
        visibility: mir::Visibility::Public,
        parameters: Vec::new(),
        locals,
        return_type: mir::Type::Int,
        entry: mir::BasicBlockId(0),
        blocks: vec![
            mir::BasicBlock {
                id: mir::BasicBlockId(0),
                instructions: vec![
                    use_value(0, integer(0)),
                    use_value(2, integer(0)),
                    use_value(3, boolean(false)),
                ],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(1),
                instructions: vec![less(0, 1, iterations)],
                terminator: mir::Terminator::Branch {
                    condition: copy(1, mir::Type::Bool),
                    then_block: mir::BasicBlockId(2),
                    else_block: mir::BasicBlockId(6),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(2),
                instructions: body,
                terminator: mir::Terminator::Branch {
                    condition: copy(3, mir::Type::Bool),
                    then_block: mir::BasicBlockId(3),
                    else_block: mir::BasicBlockId(4),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(3),
                instructions: vec![add_constant(2, 3)],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(5)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(4),
                instructions: vec![add_constant(2, 5)],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(5)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(5),
                instructions: vec![toggle(3), add_constant(0, 1)],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(6),
                instructions: Vec::new(),
                terminator: mir::Terminator::Return(Some(copy(2, mir::Type::Int))),
            },
        ],
        temporary_subregion_candidates: Vec::new(),
    })
}

fn expected_checksum(iterations: usize, scalar_operations: usize) -> ExecutionValue {
    let iterations = i32::try_from(iterations).expect("iterations fit int");
    let scalar = iterations
        .checked_mul(i32::try_from(scalar_operations).expect("scalar count fits int"))
        .expect("checksum fits int");
    let threes = iterations / 2;
    let fives = iterations - threes;
    ExecutionValue::Int(scalar + threes * 3 + fives * 5)
}

fn compute_workload(iterations: usize, scalar_operations: usize, name: &'static str) -> Workload {
    Workload {
        name,
        module: branched_loop_module(
            iterations,
            scalar_operations,
            vec![local(5, "scratch", mir::Type::Class(BOX))],
            vec![temporary_object(5), observe_object(5, 4)],
        ),
        expected: expected_checksum(iterations, scalar_operations),
    }
}

fn entity_workload(iterations: usize) -> Workload {
    Workload {
        name: "entity-processing",
        module: branched_loop_module(
            iterations,
            10,
            vec![
                local(5, "entity", mir::Type::Class(BOX)),
                local(6, "scratch", mir::Type::Array(Box::new(mir::Type::Int))),
            ],
            vec![
                temporary_object(5),
                observe_object(5, 4),
                temporary_array(6, 8),
                mir::Instruction::Assign {
                    target: mir::Place::Index {
                        array: Box::new(copy(6, mir::Type::Array(Box::new(mir::Type::Int)))),
                        index: Box::new(integer(0)),
                        element_type: mir::Type::Int,
                    },
                    value: mir::Rvalue {
                        type_: mir::Type::Int,
                        kind: mir::RvalueKind::Use(integer(7)),
                    },
                },
            ],
        ),
        expected: expected_checksum(iterations, 10),
    }
}

fn array_workload(iterations: usize, length: i32, name: &'static str) -> Workload {
    Workload {
        name,
        module: branched_loop_module(
            iterations,
            4,
            vec![local(
                5,
                "scratch",
                mir::Type::Array(Box::new(mir::Type::Int)),
            )],
            vec![temporary_array(5, length)],
        ),
        expected: expected_checksum(iterations, 4),
    }
}

fn multiple_arrays_workload(
    iterations: usize,
    count: u32,
    length: i32,
    name: &'static str,
) -> Workload {
    let locals = (0..count)
        .map(|index| {
            local(
                5 + index,
                &format!("array_{index}"),
                mir::Type::Array(Box::new(mir::Type::Int)),
            )
        })
        .collect();
    let work = (0..count)
        .map(|index| temporary_array(5 + index, length))
        .collect();
    Workload {
        name,
        module: branched_loop_module(iterations, 4, locals, work),
        expected: expected_checksum(iterations, 4),
    }
}

fn multiple_dynamic_arrays_workload(
    iterations: usize,
    count: u32,
    length: i32,
    name: &'static str,
) -> Workload {
    let mut locals = vec![local(5, "length", mir::Type::Int)];
    locals.extend((0..count).map(|index| {
        local(
            6 + index,
            &format!("array_{index}"),
            mir::Type::Array(Box::new(mir::Type::Int)),
        )
    }));
    let mut module = branched_loop_module(
        iterations,
        4,
        locals,
        (0..count)
            .map(|index| temporary_array_with_length(6 + index, 5))
            .collect(),
    );
    module.functions[0].blocks[0]
        .instructions
        .push(use_value(5, integer(length)));
    Workload {
        name,
        module,
        expected: expected_checksum(iterations, 4),
    }
}

fn string_workload(
    iterations: usize,
    name: &'static str,
    work: Vec<mir::Instruction>,
    locals: Vec<mir::Local>,
) -> Workload {
    Workload {
        name,
        module: branched_loop_module(iterations, 4, locals, work),
        expected: expected_checksum(iterations, 4),
    }
}

fn text_workload(iterations: usize) -> Workload {
    let mut work = vec![
        temporary_string(5),
        temporary_substring(6),
        temporary_join(7),
        temporary_builder(8),
        builder_append(8, copy(5, mir::Type::String)),
    ];
    for value in ["-branch-", "larger-", "construction-", "payload"] {
        work.push(builder_append(8, string(value)));
    }
    work.push(builder_snapshot(8, 9));
    Workload {
        name: "text-processing",
        module: branched_loop_module(
            iterations,
            6,
            vec![
                local(5, "formatted", mir::Type::String),
                local(6, "substring", mir::Type::String),
                local(7, "joined", mir::Type::String),
                local(8, "builder", mir::Type::Class(BUILDER)),
                local(9, "snapshot", mir::Type::String),
            ],
            work,
        ),
        expected: expected_checksum(iterations, 6),
    }
}

fn collection_workload(iterations: usize) -> Workload {
    let mut work = vec![temporary_list(5)];
    for value in 0..12 {
        work.push(list_add(5, value));
    }
    work.push(temporary_dictionary(6));
    for key in 0..12 {
        work.push(dictionary_set(6, 7, key));
    }
    Workload {
        name: "collection-processing",
        module: branched_loop_module(
            iterations,
            12,
            vec![
                local(5, "values", mir::Type::List(Box::new(mir::Type::Int))),
                local(
                    6,
                    "lookup",
                    mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int)),
                ),
                local(7, "inserted", mir::Type::Bool),
            ],
            work,
        ),
        expected: expected_checksum(iterations, 12),
    }
}

fn hidden_mixed_work() -> (Vec<mir::Local>, Vec<mir::Instruction>) {
    let locals = vec![
        local(5, "item", mir::Type::Class(BOX)),
        local(6, "array", mir::Type::Array(Box::new(mir::Type::Int))),
        local(7, "formatted", mir::Type::String),
        local(8, "builder", mir::Type::Class(BUILDER)),
        local(9, "snapshot", mir::Type::String),
        local(10, "values", mir::Type::List(Box::new(mir::Type::Int))),
        local(
            11,
            "map",
            mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int)),
        ),
        local(12, "inserted", mir::Type::Bool),
    ];
    let mut work = vec![
        temporary_object(5),
        observe_object(5, 4),
        temporary_array(6, 16),
        temporary_string(7),
        temporary_builder(8),
        builder_append(8, copy(7, mir::Type::String)),
    ];
    for value in ["-mixed-", "working-", "set-", "growth"] {
        work.push(builder_append(8, string(value)));
    }
    work.extend([
        builder_snapshot(8, 9),
        temporary_list(10),
        list_add(10, 1),
        list_add(10, 2),
        list_add(10, 3),
        list_add(10, 4),
        list_add(10, 5),
        temporary_dictionary(11),
        dictionary_set(11, 12, 1),
        dictionary_set(11, 12, 2),
        dictionary_set(11, 12, 3),
        dictionary_set(11, 12, 4),
    ]);
    (locals, work)
}

#[allow(clippy::too_many_lines)]
fn mixed_workload(iterations: usize) -> Workload {
    let outer_iterations = 100_usize.min(iterations.max(1));
    let inner_iterations = iterations / outer_iterations;
    let total_iterations = outer_iterations * inner_iterations;
    let (mut locals, mut work) = hidden_mixed_work();
    work.extend((0..16).map(|_| add_constant(2, 1)));
    locals.splice(
        0..0,
        [
            local(0, "outer_index", mir::Type::Int),
            local(1, "outer_condition", mir::Type::Bool),
            local(2, "checksum", mir::Type::Int),
            local(3, "alternate", mir::Type::Bool),
            local(4, "observed", mir::Type::Bool),
            local(13, "inner_index", mir::Type::Int),
            local(14, "inner_condition", mir::Type::Bool),
        ],
    );
    Workload {
        name: "mixed-nested",
        module: module(mir::Function {
            constructor: false,
            symbol: RUN,
            owner: None,
            name: "Run".to_owned(),
            visibility: mir::Visibility::Public,
            parameters: Vec::new(),
            locals,
            return_type: mir::Type::Int,
            entry: mir::BasicBlockId(0),
            blocks: vec![
                mir::BasicBlock {
                    id: mir::BasicBlockId(0),
                    instructions: vec![
                        use_value(0, integer(0)),
                        use_value(2, integer(0)),
                        use_value(3, boolean(false)),
                    ],
                    terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(1),
                    instructions: vec![less(0, 1, outer_iterations)],
                    terminator: mir::Terminator::Branch {
                        condition: copy(1, mir::Type::Bool),
                        then_block: mir::BasicBlockId(2),
                        else_block: mir::BasicBlockId(9),
                    },
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(2),
                    instructions: vec![use_value(13, integer(0))],
                    terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(3),
                    instructions: vec![less(13, 14, inner_iterations)],
                    terminator: mir::Terminator::Branch {
                        condition: copy(14, mir::Type::Bool),
                        then_block: mir::BasicBlockId(4),
                        else_block: mir::BasicBlockId(8),
                    },
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(4),
                    instructions: work,
                    terminator: mir::Terminator::Branch {
                        condition: copy(3, mir::Type::Bool),
                        then_block: mir::BasicBlockId(5),
                        else_block: mir::BasicBlockId(6),
                    },
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(5),
                    instructions: vec![add_constant(2, 3)],
                    terminator: mir::Terminator::Goto(mir::BasicBlockId(7)),
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(6),
                    instructions: vec![add_constant(2, 5)],
                    terminator: mir::Terminator::Goto(mir::BasicBlockId(7)),
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(7),
                    instructions: vec![toggle(3), add_constant(13, 1)],
                    terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(8),
                    instructions: vec![add_constant(0, 1)],
                    terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
                },
                mir::BasicBlock {
                    id: mir::BasicBlockId(9),
                    instructions: Vec::new(),
                    terminator: mir::Terminator::Return(Some(copy(2, mir::Type::Int))),
                },
            ],
            temporary_subregion_candidates: Vec::new(),
        }),
        expected: expected_checksum(total_iterations, 16),
    }
}

fn frame_workload(iterations: usize) -> Workload {
    let (locals, work) = hidden_mixed_work();
    Workload {
        name: "frame-like",
        module: branched_loop_module(iterations, 32, locals, work),
        expected: expected_checksum(iterations, 32),
    }
}

fn acyclic_workload() -> Workload {
    let mut function = branched_loop_module(1, 32, hidden_mixed_work().0, hidden_mixed_work().1)
        .functions
        .remove(0);
    function.blocks = vec![mir::BasicBlock {
        id: mir::BasicBlockId(0),
        instructions: {
            let mut instructions = hidden_mixed_work().1;
            instructions.extend((0..32).map(|_| add_constant(2, 1)));
            instructions
        },
        terminator: mir::Terminator::Return(Some(integer(32))),
    }];
    function.entry = mir::BasicBlockId(0);
    Workload {
        name: "acyclic-hidden",
        module: module(function),
        expected: ExecutionValue::Int(32),
    }
}

fn prepare_mode(workload: &Workload, mode: Mode) -> (mir::Module, usize, u128) {
    let mut module = workload.module.clone();
    let started = Instant::now();
    let static_regions = match mode {
        Mode::Baseline => 0,
        Mode::Raw => {
            lower_aarm_temporary_subregions_for_research(&mut module)
                .expect("raw AARM lowering succeeds")
                .subregions_lowered
        }
        Mode::ProductionV1 | Mode::ProductionV2 => {
            lower_aarm_temporary_subregions_with_policy_for_research(
                &mut module,
                if mode == Mode::ProductionV1 {
                    AarmTemporarySubregionProfitabilityPolicy::ProductionV1
                } else {
                    AarmTemporarySubregionProfitabilityPolicy::ProductionV2
                },
            )
            .expect("selector AARM lowering succeeds")
            .subregions_lowered
        }
    };
    (module, static_regions, started.elapsed().as_micros())
}

fn measure(workload: &Workload, mode: Mode) -> Measurement {
    let (module, static_regions, lowering_micros) = prepare_mode(workload, mode);
    let prepared = PreparedSequentialExecution::prepare(&module, "Run")
        .expect("benchmark JIT preparation succeeds");
    assert_eq!(
        prepared.invoke().expect("warmup executes"),
        workload.expected
    );
    let (value, stats) = prepared
        .invoke_with_stats()
        .expect("stats execution succeeds");
    assert_eq!(value, workload.expected);
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        assert_eq!(
            prepared.invoke().expect("measured execution succeeds"),
            workload.expected
        );
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    samples.sort_by(f64::total_cmp);
    #[cfg(feature = "aarm-telemetry")]
    let dynamic_regions = {
        let (value, telemetry) = prepared
            .invoke_with_aarm_telemetry()
            .expect("telemetry execution succeeds");
        assert_eq!(value, workload.expected);
        telemetry.temporary.events.rewind_events.saturating_sub(1)
    };
    #[cfg(not(feature = "aarm-telemetry"))]
    let dynamic_regions = 0;
    Measurement {
        median_ms: samples[SAMPLES / 2],
        stats,
        static_regions,
        dynamic_regions,
        lowering_micros,
    }
}

fn main() {
    let iterations = iteration_scale();
    let workloads = vec![
        compute_workload(iterations, 64, "compute-rare"),
        compute_workload(iterations, 8, "compute-moderate"),
        compute_workload(iterations, 1, "compute-frequent"),
        multiple_arrays_workload(iterations, 1, 0, "array-one-empty"),
        multiple_arrays_workload(iterations, 2, 0, "array-two-empty"),
        multiple_arrays_workload(iterations, 3, 0, "array-three-empty"),
        multiple_arrays_workload(iterations, 4, 0, "array-four-empty"),
        multiple_arrays_workload(iterations, 1, 1, "array-one-small"),
        multiple_arrays_workload(iterations, 2, 1, "array-two-small"),
        multiple_arrays_workload(iterations, 3, 1, "array-three-small"),
        multiple_arrays_workload(iterations, 4, 1, "array-four-small"),
        array_workload(iterations, 8, "array-medium"),
        array_workload(iterations, 256, "array-large"),
        multiple_dynamic_arrays_workload(iterations, 1, 1, "array-one-dynamic-small"),
        multiple_dynamic_arrays_workload(iterations, 3, 1, "array-three-dynamic-small"),
        multiple_dynamic_arrays_workload(iterations, 3, 64, "array-three-dynamic-large"),
        string_workload(
            iterations,
            "string-small",
            vec![temporary_string(5)],
            vec![local(5, "text", mir::Type::String)],
        ),
        string_workload(
            iterations,
            "string-large",
            vec![temporary_large_substring(5, 256)],
            vec![local(5, "text", mir::Type::String)],
        ),
        string_workload(
            iterations,
            "string-two-small",
            vec![temporary_string(5), temporary_string(6)],
            vec![
                local(5, "first", mir::Type::String),
                local(6, "second", mir::Type::String),
            ],
        ),
        string_workload(
            iterations,
            "string-multiple",
            vec![
                temporary_string(5),
                temporary_substring(6),
                temporary_join(7),
            ],
            vec![
                local(5, "formatted", mir::Type::String),
                local(6, "substring", mir::Type::String),
                local(7, "joined", mir::Type::String),
            ],
        ),
        entity_workload(iterations),
        text_workload(iterations),
        collection_workload(iterations),
        mixed_workload(iterations),
        frame_workload(iterations),
        acyclic_workload(),
    ];
    println!(
        "workload,mode,iterations,median_ms,peak_temporary,capacity,requested,allocations,static_regions,dynamic_regions,lowering_us"
    );
    for workload in workloads
        .into_iter()
        .filter(|workload| workload_selected(workload.name))
    {
        let mut reference = None;
        for mode in [
            Mode::Baseline,
            Mode::Raw,
            Mode::ProductionV1,
            Mode::ProductionV2,
        ] {
            let measurement = measure(&workload, mode);
            let logical = (
                measurement.stats.requested_bytes,
                measurement.stats.total_allocations,
            );
            if let Some(reference) = reference {
                assert_eq!(logical, reference, "{} changed logical work", workload.name);
            } else {
                reference = Some(logical);
            }
            println!(
                "{},{},{},{:.3},{},{},{},{},{},{},{}",
                workload.name,
                mode.name(),
                iterations,
                measurement.median_ms,
                measurement.stats.peak_used_bytes,
                measurement.stats.reserved_bytes,
                measurement.stats.requested_bytes,
                measurement.stats.total_allocations,
                measurement.static_regions,
                measurement.dynamic_regions,
                measurement.lowering_micros,
            );
        }
    }
}
