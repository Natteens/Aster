use std::fmt::Write;

use aster_compiler::{compile, compile_without_mir_optimizer_for_research, mir};

fn compile_valid(source: &str) -> aster_compiler::Compilation {
    compile(source).unwrap_or_else(|diagnostics| panic!("source must compile: {diagnostics:#?}"))
}

fn baseline(source: &str) -> aster_compiler::Compilation {
    compile_without_mir_optimizer_for_research(source)
        .unwrap_or_else(|diagnostics| panic!("baseline source must compile: {diagnostics:#?}"))
}

fn function<'a>(module: &'a mir::Module, name: &str) -> &'a mir::Function {
    module
        .functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing function `{name}`"))
}

fn instructions(function: &mir::Function) -> impl Iterator<Item = &mir::Instruction> {
    function
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
}

fn returned_integer(function: &mir::Function) -> Option<&str> {
    function.blocks.iter().find_map(|block| {
        let mir::Terminator::Return(Some(mir::Operand {
            kind: mir::OperandKind::Constant(mir::Constant::Integer(value)),
            ..
        })) = &block.terminator
        else {
            return None;
        };
        Some(value.as_str())
    })
}

#[test]
fn fixed_point_folds_propagates_copies_and_eliminates_dead_assignments() {
    let source = r"
        public int Main() {
            int seed = 10;
            int copy = seed;
            int folded = copy + 2;
            int dead = 4 * 5;
            bool condition = 3 < 4;
            if (condition) { return folded; } else { return dead; }
        }
    ";
    let baseline = baseline(source);
    let optimized = compile_valid(source);
    let baseline = function(&baseline.mir, "Main");
    let optimized = function(&optimized.mir, "Main");

    assert_eq!(baseline.blocks.len(), 3);
    assert_eq!(instructions(baseline).count(), 8);
    assert_eq!(optimized.blocks.len(), 2);
    assert_eq!(instructions(optimized).count(), 0);
    assert_eq!(returned_integer(optimized), Some("12"));
    assert!(
        optimized
            .blocks
            .iter()
            .all(|block| !matches!(block.terminator, mir::Terminator::Branch { .. }))
    );
}

#[test]
fn exact_join_propagates_but_ambiguous_join_does_not() {
    let same = compile_valid(
        "public int Pick(bool flag) { int value; if (flag) { value = 7; } else { value = 7; } return value + 1; }",
    );
    let same = function(&same.mir, "Pick");
    assert_eq!(returned_integer(same), Some("8"));

    let different = compile_valid(
        "public int Pick(bool flag) { int value; if (flag) { value = 7; } else { value = 8; } return value + 1; }",
    );
    let different = function(&different.mir, "Pick");
    assert!(instructions(different).any(|instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Binary {
                        operator: mir::BinaryOperator::Add,
                        ..
                    },
                    ..
                },
                ..
            }
        )
    }));

    let unknown = compile_valid(
        "public int Pick(bool flag, int input) { int value = input; if (flag) { value = 42; } return value + 1; }",
    );
    let unknown = function(&unknown.mir, "Pick");
    assert!(instructions(unknown).any(|instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Binary {
                        operator: mir::BinaryOperator::Add,
                        ..
                    },
                    ..
                },
                ..
            }
        )
    }));

    let newly_exact = compile_valid(
        "public int Pick() { int value; if (true) { value = 42; } else { value = 43; } return value + 1; }",
    );
    assert_eq!(
        returned_integer(function(&newly_exact.mir, "Pick")),
        Some("43")
    );
}

#[test]
fn loop_headers_meet_preheaders_and_every_backedge() {
    let source = r"
        public int WhileLoop(int count) {
            int value = 7;
            int index = 0;
            while (index < count) {
                value = value + 1;
                index = index + 1;
            }
            return value;
        }
        public int ContinueLoop(int count) {
            int value = 1;
            for (int index = 0; index < count; index += 1) {
                if (index % 2 == 0) {
                    value = value + 2;
                    continue;
                }
                value = value + 3;
            }
            return value;
        }
        public int NestedLoops(int outer) {
            int value = 3;
            int index = 0;
            while (index < outer) {
                int inner = 0;
                while (inner < 2) {
                    value = value + index + inner;
                    inner = inner + 1;
                }
                index = index + 1;
            }
            return value;
        }
    ";
    let compilation = compile_valid(source);
    for name in ["WhileLoop", "ContinueLoop", "NestedLoops"] {
        let function = function(&compilation.mir, name);
        assert!(instructions(function).any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::Binary { .. },
                        ..
                    },
                    ..
                }
            )
        }));
        assert!(function.blocks.iter().any(|block| {
            matches!(
                block.terminator,
                mir::Terminator::Goto(target) if target.0 <= block.id.0
            )
        }));
    }
}

#[test]
fn scalar_copy_keeps_the_old_value_after_source_mutation() {
    let compilation = compile_valid(
        "public int Keep(int input) { int first = input; int saved = first; input = 10; return saved; }",
    );
    let function = function(&compilation.mir, "Keep");
    let mir::Terminator::Return(Some(returned)) = &function.blocks.last().unwrap().terminator
    else {
        panic!("function must return a value")
    };
    assert!(!matches!(
        returned.kind,
        mir::OperandKind::Constant(mir::Constant::Integer(ref value)) if value == "10"
    ));
}

#[test]
fn calls_stop_scalar_facts_without_touching_aggregate_storage() {
    let compilation = compile_valid(
        "public void Observe() { } public int Run() { int value = 5; Observe(); return value + 1; }",
    );
    let run = function(&compilation.mir, "Run");
    assert!(instructions(run).any(|instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Binary { .. },
                    ..
                },
                ..
            }
        )
    }));
}

#[test]
fn scalar_copy_propagation_does_not_treat_aggregate_storage_as_ssa() {
    let scalar = compile_valid("public int Echo(int value) { int copy = value; return copy; }");
    let scalar = function(&scalar.mir, "Echo");
    assert!(instructions(scalar).next().is_none());
    assert!(matches!(
        scalar.blocks[0].terminator,
        mir::Terminator::Return(Some(mir::Operand {
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
            ..
        })) if local == scalar.parameters[0].id
    ));

    let aggregate = compile_valid(
        "public struct Pair { public int Value; } public Pair Echo(Pair value) { Pair copy = value; return copy; }",
    );
    let aggregate = function(&aggregate.mir, "Echo");
    assert!(instructions(aggregate).any(|instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                target: mir::Place::Local(_),
                value: mir::Rvalue {
                    type_: mir::Type::User(_),
                    kind: mir::RvalueKind::Use(mir::Operand {
                        kind: mir::OperandKind::Copy(mir::Place::Local(_)),
                        ..
                    })
                }
            }
        )
    }));
}

#[test]
fn runtime_integer_folding_wraps_instead_of_diagnosing_overflow() {
    let compilation = compile_valid(
        "public int Wrap() { int maximum = 2147483647; int one = 1; return maximum + one; }",
    );
    assert_eq!(
        returned_integer(function(&compilation.mir, "Wrap")),
        Some("-2147483648")
    );
}

#[test]
fn failing_allocating_calling_host_and_worker_operations_are_preserved() {
    let source = r#"
        public int Identity(int value) { return value; }
        public int Worker() { return 1; }
        public void Body(int index) { }
        public int Run(int zero, string input) {
            int pure = 1 + 2;
            int failed = 10 / zero;
            int called = Identity(5);
            int[] values = new int[1];
            string text = input + "right";
            List<int> list = new List<int>();
            int listed = list.Get(0);
            Dictionary<int, int> dictionary = new Dictionary<int, int>();
            dictionary.Add(1, 2);
            Log("kept");
            Task<int> task = Task.Run(Worker);
            Parallel.For(0, 1, Body);
            return 0;
        }
    "#;
    let compilation = compile_valid(source);
    let run = function(&compilation.mir, "Run");
    assert!(!instructions(run).any(|instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Use(mir::Operand {
                        kind: mir::OperandKind::Constant(mir::Constant::Integer(value)),
                        ..
                    }),
                    ..
                },
                ..
            } if value == "3"
        )
    }));
    assert!(instructions(run).any(|instruction| {
        matches!(
            instruction,
            mir::Instruction::Assign {
                value: mir::Rvalue {
                    kind: mir::RvalueKind::Binary {
                        operator: mir::BinaryOperator::Divide,
                        ..
                    },
                    ..
                },
                ..
            }
        )
    }));
    assert!(
        instructions(run).any(|instruction| matches!(instruction, mir::Instruction::Call { .. }))
    );
    assert!(
        instructions(run)
            .any(|instruction| matches!(instruction, mir::Instruction::AllocateArray { .. }))
    );
    assert!(
        instructions(run)
            .any(|instruction| { matches!(instruction, mir::Instruction::AllocateList { .. }) })
    );
    assert!(
        instructions(run).any(|instruction| {
            matches!(instruction, mir::Instruction::AllocateDictionary { .. })
        })
    );
    assert!(
        instructions(run)
            .any(|instruction| { matches!(instruction, mir::Instruction::ListGet { .. }) })
    );
    assert!(
        instructions(run)
            .any(|instruction| { matches!(instruction, mir::Instruction::DictionaryAdd { .. }) })
    );
    assert!(instructions(run).any(|instruction| {
        matches!(
            instruction,
            mir::Instruction::CallIntrinsic {
                intrinsic: mir::Intrinsic::StringConcat | mir::Intrinsic::StringConcatTemporary,
                ..
            }
        )
    }));
    for expected in [
        mir::Intrinsic::Log,
        mir::Intrinsic::TaskRun,
        mir::Intrinsic::ParallelFor,
    ] {
        assert!(instructions(run).any(|instruction| {
            matches!(instruction, mir::Instruction::CallIntrinsic { intrinsic, .. } if *intrinsic == expected)
        }));
    }
}

#[test]
fn scalar_facts_stop_at_async_and_worker_transfer_boundaries() {
    let compilation = compile_valid(
        "public int Compute() { return 1; } public async Task<int> Run() { int value = 5; int result = await Task.Run(Compute); return value + result; }",
    );
    let stored = compilation
        .mir
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .find_map(|instruction| {
            let mir::Instruction::CallIntrinsic {
                intrinsic: mir::Intrinsic::AsyncStoreSlot,
                arguments,
                ..
            } = instruction
            else {
                return None;
            };
            arguments.get(2)
        })
        .expect("async lowering stores the live scalar in its frame");
    assert!(matches!(
        stored.kind,
        mir::OperandKind::Copy(mir::Place::Local(_))
    ));
}

#[test]
fn constant_nested_control_removes_unreachable_blocks_and_jump_chains() {
    let source = "public int Main() { if (true) { if (false) { return 1; } return 2; } return 3; }";
    let baseline = baseline(source);
    let optimized = compile_valid(source);
    let baseline = function(&baseline.mir, "Main");
    let optimized = function(&optimized.mir, "Main");
    assert!(optimized.blocks.len() < baseline.blocks.len());
    assert_eq!(returned_integer(optimized), Some("2"));
    assert!(
        optimized
            .blocks
            .iter()
            .all(|block| !matches!(block.terminator, mir::Terminator::Branch { .. }))
    );
}

#[test]
fn dependent_cfg_chain_reaches_its_structural_fixed_point() {
    let mut source = String::from("public int Main() { bool condition = true; int value = 0;");
    for value in 1..=16 {
        write!(
            source,
            "if (condition) {{ value = {value}; }} else {{ value = -{value}; }} condition = value == {value};"
        )
        .expect("write source");
    }
    source.push_str("return value; }");

    let optimized = compile_valid(&source);
    let optimized = function(&optimized.mir, "Main");
    assert_eq!(returned_integer(optimized), Some("16"));
    assert!(
        optimized
            .blocks
            .iter()
            .all(|block| !matches!(block.terminator, mir::Terminator::Branch { .. }))
    );
}

#[test]
fn known_enum_tag_uses_ordinary_mir_constant_and_cfg_folding() {
    let source = r"
        public enum Kind { A, B, C }
        public int Main() {
            Kind value = Kind.B;
            switch (value) {
                case A: return 1;
                case B: return 2;
                case C: return 3;
            }
        }
    ";
    let baseline = baseline(source);
    let optimized = compile_valid(source);
    let baseline = function(&baseline.mir, "Main");
    let optimized = function(&optimized.mir, "Main");
    assert!(
        optimized
            .blocks
            .iter()
            .filter(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
            .count()
            < baseline
                .blocks
                .iter()
                .filter(|block| matches!(block.terminator, mir::Terminator::Branch { .. }))
                .count()
    );
    assert_eq!(returned_integer(optimized), Some("2"));
}

#[test]
fn optimized_mir_is_deterministic() {
    let source = r"
        public enum Kind { A, B }
        public int Main(bool flag, int count) {
            int value = 3;
            int index = 0;
            while (index < count) {
                if (flag) { value = value + 1; } else { value = value + 2; }
                index = index + 1;
            }
            Kind kind = Kind.B;
            switch (kind) { case A: return value; case B: return value + 2; }
        }
    ";
    let expected = compile_valid(source).mir.to_string();
    for _ in 0..10 {
        assert_eq!(compile_valid(source).mir.to_string(), expected);
    }
}
