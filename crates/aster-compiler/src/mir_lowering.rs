mod calls;
mod control_flow;
mod expressions;
mod module;
mod places;

use std::collections::HashMap;

use aster_hir as hir;
use aster_mir as mir;

pub(crate) fn lower(module: &hir::Module) -> mir::Module {
    module::lower(module)
}

struct PendingBlock {
    id: mir::BasicBlockId,
    instructions: Vec<mir::Instruction>,
    terminator: Option<mir::Terminator>,
}

#[derive(Clone, Copy)]
struct LoopTargets {
    break_block: mir::BasicBlockId,
    continue_block: mir::BasicBlockId,
}

struct FunctionLowerer {
    blocks: Vec<PendingBlock>,
    current: Option<mir::BasicBlockId>,
    parameters: Vec<mir::Local>,
    locals: Vec<mir::Local>,
    symbol_locals: HashMap<hir::SymbolId, mir::LocalId>,
    loops: Vec<LoopTargets>,
    next_local: u32,
    next_temporary: u32,
    intrinsics: HashMap<hir::SymbolId, hir::Intrinsic>,
    enums: HashMap<hir::SymbolId, mir::EnumDefinition>,
}

impl FunctionLowerer {
    fn new(
        function: &hir::Function,
        intrinsics: HashMap<hir::SymbolId, hir::Intrinsic>,
        enums: HashMap<hir::SymbolId, mir::EnumDefinition>,
    ) -> Self {
        let mut lowerer = Self {
            blocks: Vec::new(),
            current: None,
            parameters: Vec::new(),
            locals: Vec::new(),
            symbol_locals: HashMap::new(),
            loops: Vec::new(),
            next_local: 0,
            next_temporary: 0,
            intrinsics,
            enums,
        };
        let entry = lowerer.new_block();
        lowerer.current = Some(entry);
        for parameter in &function.parameters {
            let local = lowerer.source_local(
                parameter.symbol,
                parameter.name.clone(),
                parameter.type_.clone(),
                true,
            );
            lowerer.parameters.push(local);
        }
        lowerer
    }

    fn lower(mut self, function: &hir::Function, owner: Option<hir::SymbolId>) -> mir::Function {
        // Sublote 1 stops at HIR: async bodies (which contain `await`) are not
        // lowered to executable MIR yet. Instead of walking the body, emit a
        // non-executable placeholder that reports a controlled runtime error if
        // the function is ever executed, then returns. This keeps `await` out of
        // MIR lowering entirely and, unlike a trap, never aborts the host.
        if function.is_async {
            self.instruction(mir::Instruction::CallIntrinsic {
                destination: None,
                intrinsic: mir::Intrinsic::ReportRuntimeError(
                    mir::RuntimeErrorKind::AsyncRuntimeUnavailable,
                ),
                arguments: Vec::new(),
                return_type: mir::Type::Void,
            });
            // `Task<T>` is a plain `i64` handle, so a zero long is a valid,
            // never-observed return value: execution stops at the controlled
            // error above before this handle can be used.
            self.terminate_current(mir::Terminator::Return(Some(mir::Operand {
                type_: mir::Type::Long,
                kind: mir::OperandKind::Constant(mir::Constant::Integer("0".to_owned())),
            })));
        } else {
            if let Some(body) = &function.body {
                self.lower_block(body);
            }
            if let Some(current) = self.current {
                self.terminate(current, mir::Terminator::End);
            }
        }
        let blocks = self
            .blocks
            .into_iter()
            .map(|block| mir::BasicBlock {
                id: block.id,
                instructions: block.instructions,
                terminator: block.terminator.unwrap_or(mir::Terminator::End),
            })
            .collect();
        mir::Function {
            constructor: function.constructor,
            symbol: function.symbol,
            owner,
            name: function.name.clone(),
            visibility: function.visibility,
            parameters: self.parameters,
            locals: self.locals,
            return_type: function.return_type.clone(),
            entry: mir::BasicBlockId(0),
            blocks,
        }
    }

    fn temporary(&mut self, type_: hir::Type, kind: mir::RvalueKind) -> mir::Operand {
        let local = self.new_temporary(type_.clone());
        self.assign(
            mir::Place::Local(local),
            mir::Rvalue {
                type_: type_.clone(),
                kind,
            },
        );
        mir::Operand {
            type_,
            kind: mir::OperandKind::Copy(mir::Place::Local(local)),
        }
    }

    fn new_temporary(&mut self, type_: hir::Type) -> mir::LocalId {
        let id = self.allocate_local();
        let name = format!("_tmp{}", self.next_temporary);
        self.next_temporary += 1;
        self.locals.push(mir::Local {
            id,
            symbol: None,
            name,
            type_,
            mutable: true,
            temporary: true,
        });
        id
    }

    fn source_local(
        &mut self,
        symbol: hir::SymbolId,
        name: String,
        type_: hir::Type,
        mutable: bool,
    ) -> mir::Local {
        let id = self.allocate_local();
        self.symbol_locals.insert(symbol, id);
        mir::Local {
            id,
            symbol: Some(symbol),
            name,
            type_,
            mutable,
            temporary: false,
        }
    }

    fn allocate_local(&mut self) -> mir::LocalId {
        let id = mir::LocalId(self.next_local);
        self.next_local += 1;
        id
    }

    fn new_block(&mut self) -> mir::BasicBlockId {
        let id =
            mir::BasicBlockId(u32::try_from(self.blocks.len()).expect("MIR block count fits u32"));
        self.blocks.push(PendingBlock {
            id,
            instructions: Vec::new(),
            terminator: None,
        });
        id
    }

    fn assign(&mut self, target: mir::Place, value: mir::Rvalue) {
        self.instruction(mir::Instruction::Assign { target, value });
    }

    fn instruction(&mut self, instruction: mir::Instruction) {
        let current = self.current.expect("instructions require a live block");
        self.blocks[current.0 as usize]
            .instructions
            .push(instruction);
    }

    fn terminate_current(&mut self, terminator: mir::Terminator) {
        let current = self
            .current
            .take()
            .expect("terminator requires a live block");
        self.terminate(current, terminator);
    }

    fn terminate(&mut self, block: mir::BasicBlockId, terminator: mir::Terminator) {
        let slot = &mut self.blocks[block.0 as usize].terminator;
        assert!(slot.is_none(), "a basic block has one terminator");
        *slot = Some(terminator);
    }
}
