//! Manual release-only AARM iteration-local loop comparison.
//!
//! This intentionally uses the exact supported backend-neutral MIR subset:
//! object/array allocation, scalar loop state, one header, and one latch.
//! Timings are informational and are never CI assertions.

use std::time::Instant;

use aster_codegen_cranelift::{ExecutionValue, MemoryStats, execute, execute_with_stats};
use aster_compiler::lower_aarm_temporary_subregions_for_research;
use aster_mir as mir;

const RUN: mir::SymbolId = mir::SymbolId(1);
const WORK: mir::SymbolId = mir::SymbolId(2);
const BOX: mir::SymbolId = mir::SymbolId(10);
const FIELD: mir::SymbolId = mir::SymbolId(11);
const SAMPLES: usize = 5;

#[derive(Clone, Copy)]
enum AllocationShape {
    Object,
    Array,
}

impl AllocationShape {
    const fn name(self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Array => "array",
        }
    }

    fn type_(self) -> mir::Type {
        match self {
            Self::Object => mir::Type::Class(BOX),
            Self::Array => mir::Type::Array(Box::new(mir::Type::Int)),
        }
    }

    fn allocation(self, local: u32) -> mir::Instruction {
        match self {
            Self::Object => mir::Instruction::AllocateObject {
                destination: mir::Place::Local(mir::LocalId(local)),
                class: BOX,
                region: mir::AllocationRegion::Temporary,
            },
            Self::Array => mir::Instruction::AllocateArray {
                destination: mir::Place::Local(mir::LocalId(local)),
                element_type: mir::Type::Int,
                length: integer(8),
                requires_default: false,
                region: mir::AllocationRegion::Temporary,
            },
        }
    }
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

fn copy(local: u32, type_: mir::Type) -> mir::Operand {
    mir::Operand {
        type_,
        kind: mir::OperandKind::Copy(mir::Place::Local(mir::LocalId(local))),
    }
}

fn assign(target: u32, value: mir::Rvalue) -> mir::Instruction {
    mir::Instruction::Assign {
        target: mir::Place::Local(mir::LocalId(target)),
        value,
    }
}

fn use_integer(value: i32) -> mir::Rvalue {
    mir::Rvalue {
        type_: mir::Type::Int,
        kind: mir::RvalueKind::Use(integer(value)),
    }
}

fn less(index: u32, condition: u32, iterations: usize) -> mir::Instruction {
    assign(
        condition,
        mir::Rvalue {
            type_: mir::Type::Bool,
            kind: mir::RvalueKind::Binary {
                left: copy(index, mir::Type::Int),
                operator: mir::BinaryOperator::Less,
                right: integer(i32::try_from(iterations).expect("benchmark iterations fit in int")),
            },
        },
    )
}

fn increment(index: u32) -> mir::Instruction {
    assign(
        index,
        mir::Rvalue {
            type_: mir::Type::Int,
            kind: mir::RvalueKind::Binary {
                left: copy(index, mir::Type::Int),
                operator: mir::BinaryOperator::Add,
                right: integer(1),
            },
        },
    )
}

fn function(
    symbol: mir::SymbolId,
    name: &str,
    locals: Vec<mir::Local>,
    return_type: mir::Type,
    blocks: Vec<mir::BasicBlock>,
) -> mir::Function {
    mir::Function {
        constructor: false,
        symbol,
        owner: None,
        name: name.to_owned(),
        visibility: mir::Visibility::Public,
        parameters: Vec::new(),
        locals,
        return_type,
        entry: mir::BasicBlockId(0),
        blocks,
        temporary_subregion_candidates: Vec::new(),
    }
}

fn module(iterations: usize, shape: AllocationShape, helper_scoped: bool) -> mir::Module {
    let body = if helper_scoped {
        vec![mir::Instruction::Call {
            destination: None,
            function: WORK,
            arguments: Vec::new(),
            return_type: mir::Type::Void,
        }]
    } else {
        vec![shape.allocation(0)]
    };
    let run = function(
        RUN,
        "Run",
        vec![
            local(0, "value", shape.type_()),
            local(1, "index", mir::Type::Int),
            local(2, "condition", mir::Type::Bool),
        ],
        mir::Type::Int,
        vec![
            mir::BasicBlock {
                id: mir::BasicBlockId(0),
                instructions: vec![assign(1, use_integer(0))],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(1),
                instructions: vec![less(1, 2, iterations)],
                terminator: mir::Terminator::Branch {
                    condition: copy(2, mir::Type::Bool),
                    then_block: mir::BasicBlockId(2),
                    else_block: mir::BasicBlockId(4),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(2),
                instructions: body,
                terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(3),
                instructions: vec![increment(1)],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(4),
                instructions: Vec::new(),
                terminator: mir::Terminator::Return(Some(copy(1, mir::Type::Int))),
            },
        ],
    );
    let mut functions = vec![run];
    if helper_scoped {
        functions.push(function(
            WORK,
            "Work",
            vec![local(0, "value", shape.type_())],
            mir::Type::Void,
            vec![mir::BasicBlock {
                id: mir::BasicBlockId(0),
                instructions: vec![shape.allocation(0)],
                terminator: mir::Terminator::Return(None),
            }],
        ));
    }
    mir::Module {
        structs: Vec::new(),
        classes: vec![mir::ClassDefinition {
            symbol: BOX,
            name: "Box".to_owned(),
            fields: vec![mir::FieldDefinition {
                symbol: FIELD,
                name: "value".to_owned(),
                type_: mir::Type::Int,
            }],
        }],
        interfaces: Vec::new(),
        enums: Vec::new(),
        interface_implementations: Vec::new(),
        functions,
    }
}

fn nested_module(
    outer_iterations: usize,
    inner_iterations: usize,
    shape: AllocationShape,
) -> mir::Module {
    let run = function(
        RUN,
        "Run",
        vec![
            local(0, "inner_value", shape.type_()),
            local(1, "outer_index", mir::Type::Int),
            local(2, "inner_index", mir::Type::Int),
            local(3, "condition", mir::Type::Bool),
        ],
        mir::Type::Int,
        vec![
            mir::BasicBlock {
                id: mir::BasicBlockId(0),
                instructions: vec![assign(1, use_integer(0))],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(1),
                instructions: vec![less(1, 3, outer_iterations)],
                terminator: mir::Terminator::Branch {
                    condition: copy(3, mir::Type::Bool),
                    then_block: mir::BasicBlockId(2),
                    else_block: mir::BasicBlockId(6),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(2),
                instructions: vec![assign(2, use_integer(0))],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(3),
                instructions: vec![less(2, 3, inner_iterations)],
                terminator: mir::Terminator::Branch {
                    condition: copy(3, mir::Type::Bool),
                    then_block: mir::BasicBlockId(4),
                    else_block: mir::BasicBlockId(5),
                },
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(4),
                instructions: vec![shape.allocation(0), increment(2)],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(3)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(5),
                instructions: vec![increment(1)],
                terminator: mir::Terminator::Goto(mir::BasicBlockId(1)),
            },
            mir::BasicBlock {
                id: mir::BasicBlockId(6),
                instructions: Vec::new(),
                terminator: mir::Terminator::Return(Some(copy(1, mir::Type::Int))),
            },
        ],
    );
    mir::Module {
        structs: Vec::new(),
        classes: vec![mir::ClassDefinition {
            symbol: BOX,
            name: "Box".to_owned(),
            fields: vec![mir::FieldDefinition {
                symbol: FIELD,
                name: "value".to_owned(),
                type_: mir::Type::Int,
            }],
        }],
        interfaces: Vec::new(),
        enums: Vec::new(),
        interface_implementations: Vec::new(),
        functions: vec![run],
    }
}

fn run_case(
    iterations: usize,
    shape: AllocationShape,
    helper_scoped: bool,
    iteration_reclaim: bool,
) -> (f64, MemoryStats) {
    let mut module = module(iterations, shape, helper_scoped);
    if iteration_reclaim {
        let report = lower_aarm_temporary_subregions_for_research(&mut module)
            .expect("AARM-5E2A lowers the direct loop");
        assert_eq!(report.subregions_lowered, 1);
    }
    let expected = ExecutionValue::Int(i32::try_from(iterations).expect("iterations fit in int"));
    let (value, stats) = execute_with_stats(&module, "Run").expect("benchmark executes");
    assert_eq!(value, expected);
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let value = execute(&module, "Run").expect("benchmark executes");
        assert_eq!(value, expected);
        samples.push(start.elapsed().as_secs_f64() * 1_000.0);
    }
    samples.sort_by(f64::total_cmp);
    (samples[SAMPLES / 2], stats)
}

fn run_nested_case(
    outer_iterations: usize,
    inner_iterations: usize,
    shape: AllocationShape,
    iteration_reclaim: bool,
) -> (f64, MemoryStats) {
    let mut module = nested_module(outer_iterations, inner_iterations, shape);
    if iteration_reclaim {
        let report = lower_aarm_temporary_subregions_for_research(&mut module)
            .expect("AARM-5E2B2A lowers the nested leaf loop");
        assert_eq!(report.subregions_lowered, 1);
    }
    let expected = ExecutionValue::Int(i32::try_from(outer_iterations).expect("iterations fit"));
    let (value, stats) = execute_with_stats(&module, "Run").expect("benchmark executes");
    assert_eq!(value, expected);
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        assert_eq!(
            execute(&module, "Run").expect("benchmark executes"),
            expected
        );
        samples.push(start.elapsed().as_secs_f64() * 1_000.0);
    }
    samples.sort_by(f64::total_cmp);
    (samples[SAMPLES / 2], stats)
}

fn main() {
    for iterations in [100_000, 1_000_000, 4_000_000] {
        for shape in [AllocationShape::Object, AllocationShape::Array] {
            for (variant, helper_scoped, iteration_reclaim) in [
                ("direct", false, false),
                ("helper", true, false),
                ("aarm", false, true),
            ] {
                let (median_ms, stats) =
                    run_case(iterations, shape, helper_scoped, iteration_reclaim);
                println!(
                    "shape={:<6} variant={variant:<6} iterations={iterations:<7} median_ms={median_ms:>9.3} allocations={:>8} requested={:>10} peak_used={:>10} capacity={:>10}",
                    shape.name(),
                    stats.total_allocations,
                    stats.requested_bytes,
                    stats.peak_used_bytes,
                    stats.reserved_bytes,
                );
            }
        }
    }
    for shape in [AllocationShape::Object, AllocationShape::Array] {
        for (variant, iteration_reclaim) in [("nested-direct", false), ("nested-aarm", true)] {
            let (median_ms, stats) = run_nested_case(1_000, 1_000, shape, iteration_reclaim);
            println!(
                "shape={:<6} variant={variant:<13} outer=1000 inner=1000 allocations={:>8} median_ms={median_ms:>9.3} requested={:>10} peak_used={:>10} capacity={:>10}",
                shape.name(),
                stats.total_allocations,
                stats.requested_bytes,
                stats.peak_used_bytes,
                stats.reserved_bytes,
            );
        }
    }
}
