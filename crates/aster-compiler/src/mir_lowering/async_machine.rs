//! Dedicated lowering for the restricted v1 `async`/`await` surface.
//!
//! A valid async function is linear, has exactly one `await Task.Run(F)`, and
//! returns `Task<T>` with a scalar `T`. It is lowered to two MIR functions:
//!
//! * a **wrapper** keeping the original symbol, which only registers the state
//!   machine and returns its `Task<T>` handle — it never runs the body;
//! * a **`MoveNext`** function with a fresh, unique `SymbolId`, referenced only
//!   by identity, taking the async task's handle as a hidden scalar parameter
//!   and implementing a two-state machine (`0` before the suspension point,
//!   `1` after it).
//!
//! State `0` runs the code before the `await`, saves the live scalar locals to
//! the host-owned frame, submits the inner `Task.Run` target with a completion
//! token, advances to state `1`, and returns `Pending`. State `1` restores the
//! saved locals, materializes the completed inner result, runs the code after
//! the `await`, publishes the final value as the task's candidate result, and
//! returns `Completed`.

use super::{Arc, FunctionLowerer, HashMap, hir, mir};

/// `MoveNext` status returned to the host pump. Never a `Task<T>` value.
const PENDING: i64 = 0;
const COMPLETED: i64 = 1;

impl FunctionLowerer {
    /// Publish the async task's candidate result (if any) and return the
    /// machine's `Completed` status instead of the source value. Only called
    /// while `async_handle` is set (see `control_flow::lower_statement`).
    pub(super) fn emit_async_return(&mut self, value: Option<mir::Operand>) {
        let handle = self
            .async_handle
            .clone()
            .expect("emit_async_return is only reachable inside a MoveNext");
        if let Some(value) = value {
            self.instruction(mir::Instruction::CallIntrinsic {
                destination: None,
                intrinsic: mir::Intrinsic::AsyncSetResult,
                arguments: vec![handle, value],
                return_type: mir::Type::Void,
            });
        }
        self.terminate_current(mir::Terminator::Return(Some(status(COMPLETED))));
    }
}

/// Lower one validated async function into its `(wrapper, move_next)` pair.
pub(super) fn lower(
    function: &hir::Function,
    owner: Option<hir::SymbolId>,
    intrinsics: &HashMap<hir::SymbolId, hir::Intrinsic>,
    enum_cases: &Arc<HashMap<hir::SymbolId, mir::EnumCaseDefinition>>,
    move_next_symbol: hir::SymbolId,
) -> (mir::Function, mir::Function) {
    let body = function
        .body
        .as_ref()
        .expect("a validated async function has a body");
    let plan = AsyncPlan::new(body);

    let move_next = lower_move_next(function, &plan, intrinsics, enum_cases, move_next_symbol);
    let wrapper = lower_wrapper(function, owner, move_next_symbol, plan.slots.len());
    (wrapper, move_next)
}

/// The static shape of an async body: where the single `await` lives, which
/// inner target it spawns, its scalar result type, and the scalar locals
/// declared before it (conservatively all persisted across the suspension).
struct AsyncPlan<'a> {
    await_index: usize,
    inner_symbol: hir::SymbolId,
    result_type: hir::Type,
    slots: Vec<(hir::SymbolId, hir::Type)>,
    statements: &'a [hir::Statement],
}

impl<'a> AsyncPlan<'a> {
    fn new(body: &'a hir::Block) -> Self {
        let statements = body.statements.as_slice();
        let await_index = statements
            .iter()
            .position(statement_contains_await)
            .expect("a validated async body contains exactly one await");
        let (inner_symbol, result_type) = statements[..=await_index]
            .iter()
            .find_map(statement_await)
            .expect("the await statement names an inner Task.Run target");
        let slots = statements[..await_index]
            .iter()
            .filter_map(|statement| match statement {
                hir::Statement::Variable(variable) => {
                    Some((variable.symbol, variable.type_.clone()))
                }
                _ => None,
            })
            .collect();
        Self {
            await_index,
            inner_symbol,
            result_type,
            slots,
            statements,
        }
    }
}

fn lower_wrapper(
    function: &hir::Function,
    owner: Option<hir::SymbolId>,
    move_next_symbol: hir::SymbolId,
    slot_count: usize,
) -> mir::Function {
    let mut lowerer = FunctionLowerer::new_bare(HashMap::new(), Arc::new(HashMap::new()));
    let handle = mir::Place::Local(lowerer.new_temporary(function.return_type.clone()));
    lowerer.instruction(mir::Instruction::CallIntrinsic {
        destination: Some(handle.clone()),
        intrinsic: mir::Intrinsic::AsyncSpawn,
        arguments: vec![
            mir::Operand {
                type_: mir::Type::Long,
                kind: mir::OperandKind::Function(move_next_symbol),
            },
            int_operand(slot_count),
        ],
        return_type: function.return_type.clone(),
    });
    lowerer.terminate_current(mir::Terminator::Return(Some(mir::Operand {
        type_: function.return_type.clone(),
        kind: mir::OperandKind::Copy(handle),
    })));
    lowerer.finish(
        function.symbol,
        owner,
        function.name.clone(),
        function.visibility,
        function.return_type.clone(),
    )
}

#[allow(clippy::too_many_lines)]
fn lower_move_next(
    function: &hir::Function,
    plan: &AsyncPlan<'_>,
    intrinsics: &HashMap<hir::SymbolId, hir::Intrinsic>,
    enum_cases: &Arc<HashMap<hir::SymbolId, mir::EnumCaseDefinition>>,
    move_next_symbol: hir::SymbolId,
) -> mir::Function {
    let mut lowerer = FunctionLowerer::new_bare(intrinsics.clone(), Arc::clone(enum_cases));

    // Hidden scalar parameter carrying the outer async task's handle. It has
    // no source symbol, participates in no overload, and is validated
    // structurally as a plain `Long` (ABI-compatible with a `TaskHandleId`).
    let handle_id = lowerer.allocate_local();
    let handle_local = mir::Local {
        id: handle_id,
        symbol: None,
        name: "$handle".to_owned(),
        type_: mir::Type::Long,
        mutable: false,
        temporary: false,
    };
    lowerer.push_parameter(handle_local);
    let handle = mir::Operand {
        type_: mir::Type::Long,
        kind: mir::OperandKind::Copy(mir::Place::Local(handle_id)),
    };
    lowerer.async_handle = Some(handle.clone());

    // entry: read state, branch on `state == 0`.
    let state = lowerer.temporary_intrinsic(
        mir::Intrinsic::AsyncState,
        vec![handle.clone()],
        mir::Type::Int,
    );
    let is_state_zero = lowerer.temporary(
        mir::Type::Bool,
        mir::RvalueKind::Binary {
            left: state,
            operator: mir::BinaryOperator::Equal,
            right: int_operand(0),
        },
    );
    let state0 = lowerer.new_block();
    let state1 = lowerer.new_block();
    lowerer.terminate_current(mir::Terminator::Branch {
        condition: is_state_zero,
        then_block: state0,
        else_block: state1,
    });

    // STATE 0: pre-await code, persist locals, spawn inner, suspend.
    lowerer.current = Some(state0);
    for statement in &plan.statements[..plan.await_index] {
        if lowerer.current.is_none() {
            break;
        }
        lowerer.lower_statement(statement);
    }
    if lowerer.current.is_some() {
        for (index, (symbol, type_)) in plan.slots.iter().enumerate() {
            let local = lowerer.symbol_local(*symbol);
            lowerer.instruction(mir::Instruction::CallIntrinsic {
                destination: None,
                intrinsic: mir::Intrinsic::AsyncStoreSlot,
                arguments: vec![
                    handle.clone(),
                    int_operand(index),
                    mir::Operand {
                        type_: type_.clone(),
                        kind: mir::OperandKind::Copy(mir::Place::Local(local)),
                    },
                ],
                return_type: mir::Type::Void,
            });
        }
        lowerer.instruction(mir::Instruction::CallIntrinsic {
            destination: None,
            intrinsic: mir::Intrinsic::AsyncSpawnInner,
            arguments: vec![
                handle.clone(),
                mir::Operand {
                    type_: plan.result_type.clone(),
                    kind: mir::OperandKind::Function(plan.inner_symbol),
                },
            ],
            return_type: mir::Type::Void,
        });
        lowerer.instruction(mir::Instruction::CallIntrinsic {
            destination: None,
            intrinsic: mir::Intrinsic::AsyncSetState,
            arguments: vec![handle.clone(), int_operand(1)],
            return_type: mir::Type::Void,
        });
        lowerer.terminate_current(mir::Terminator::Return(Some(status(PENDING))));
    }

    // STATE 1: restore locals, materialize the await result, run the rest.
    lowerer.current = Some(state1);
    for (index, (symbol, type_)) in plan.slots.iter().enumerate() {
        let local = lowerer.symbol_local(*symbol);
        lowerer.instruction(mir::Instruction::CallIntrinsic {
            destination: Some(mir::Place::Local(local)),
            intrinsic: mir::Intrinsic::AsyncLoadSlot,
            arguments: vec![handle.clone(), int_operand(index)],
            return_type: type_.clone(),
        });
    }
    let result = lowerer.temporary_intrinsic(
        mir::Intrinsic::AsyncAwaitResult,
        vec![handle.clone()],
        plan.result_type.clone(),
    );
    lowerer.async_await_result = Some(result);
    for statement in &plan.statements[plan.await_index..] {
        if lowerer.current.is_none() {
            break;
        }
        lowerer.lower_statement(statement);
    }
    if lowerer.current.is_some() {
        // A validated async body always returns before falling through, but
        // stay defensive: complete with whatever candidate result was set.
        lowerer.terminate_current(mir::Terminator::Return(Some(status(COMPLETED))));
    }

    let mut move_next = lowerer.finish(
        move_next_symbol,
        None,
        format!("{}$MoveNext", function.name),
        // Never public: not exported, not selectable as an entry point.
        mir::Visibility::Private,
        mir::Type::Int,
    );
    move_next.owner = None;
    move_next
}

fn statement_contains_await(statement: &hir::Statement) -> bool {
    statement_await(statement).is_some()
}

/// The `(inner Task.Run target, awaited scalar result type)` of the single
/// `await` inside `statement`, if it contains one.
fn statement_await(statement: &hir::Statement) -> Option<(hir::SymbolId, hir::Type)> {
    match statement {
        hir::Statement::Variable(variable) => {
            variable.initializer.as_ref().and_then(expression_await)
        }
        hir::Statement::Return(value) => value.as_ref().and_then(expression_await),
        hir::Statement::Expression(expression) => expression_await(expression),
        _ => None,
    }
}

fn expression_await(expression: &hir::Expression) -> Option<(hir::SymbolId, hir::Type)> {
    match &expression.kind {
        hir::ExpressionKind::Await {
            operand,
            result_type,
        } => {
            let hir::ExpressionKind::TaskRun { function, .. } = &operand.kind else {
                return None;
            };
            Some((*function, (**result_type).clone()))
        }
        hir::ExpressionKind::Convert { operand } | hir::ExpressionKind::Unary { operand, .. } => {
            expression_await(operand)
        }
        hir::ExpressionKind::Binary { left, right, .. } => {
            expression_await(left).or_else(|| expression_await(right))
        }
        hir::ExpressionKind::Assignment { target, value, .. } => {
            expression_await(target).or_else(|| expression_await(value))
        }
        _ => None,
    }
}

fn int_operand(value: usize) -> mir::Operand {
    mir::Operand {
        type_: mir::Type::Int,
        kind: mir::OperandKind::Constant(mir::Constant::Integer(value.to_string())),
    }
}

fn status(value: i64) -> mir::Operand {
    mir::Operand {
        type_: mir::Type::Int,
        kind: mir::OperandKind::Constant(mir::Constant::Integer(value.to_string())),
    }
}
