use super::expressions::{expression_symbol, lower_intrinsic};
use super::{FunctionLowerer, hir, mir};

impl FunctionLowerer {
    pub(super) fn lower_call(
        &mut self,
        callee: &hir::Expression,
        arguments: &[hir::Expression],
        return_type: &hir::Type,
    ) -> Option<mir::Operand> {
        let function = expression_symbol(callee).expect("validated call has a resolved symbol");
        if let Some(intrinsic) = self.intrinsics.get(&function).copied() {
            debug_assert!(
                arguments.is_empty(),
                "validated runtime-error intrinsic is nullary"
            );
            self.instruction(mir::Instruction::CallIntrinsic {
                destination: None,
                intrinsic: lower_intrinsic(intrinsic),
                arguments: Vec::new(),
                return_type: mir::Type::Void,
            });
            return None;
        }
        if let hir::ExpressionKind::Member { object, .. } = &callee.kind
            && matches!(object.type_, hir::Type::Interface(_))
        {
            let receiver = self
                .lower_expression(object)
                .expect("interface method receiver produces a value");
            let lowered_arguments = arguments
                .iter()
                .filter_map(|argument| self.lower_expression(argument))
                .collect();
            if return_type == &hir::Type::Void {
                self.instruction(mir::Instruction::CallInterface {
                    destination: None,
                    receiver,
                    method: function,
                    arguments: lowered_arguments,
                    return_type: mir::Type::Void,
                });
                return None;
            }
            let local = self.new_temporary(return_type.clone());
            let destination = mir::Place::Local(local);
            self.instruction(mir::Instruction::CallInterface {
                destination: Some(destination.clone()),
                receiver,
                method: function,
                arguments: lowered_arguments,
                return_type: return_type.clone(),
            });
            return Some(mir::Operand {
                type_: return_type.clone(),
                kind: mir::OperandKind::Copy(destination),
            });
        }
        let mut lowered_arguments = Vec::new();
        if let hir::ExpressionKind::Member { object, .. } = &callee.kind {
            lowered_arguments.push(
                self.lower_expression(object)
                    .expect("method receiver produces a value"),
            );
        }
        lowered_arguments.extend(
            arguments
                .iter()
                .filter_map(|argument| self.lower_expression(argument)),
        );
        if return_type == &hir::Type::Void {
            self.instruction(mir::Instruction::Call {
                destination: None,
                function,
                arguments: lowered_arguments,
                return_type: mir::Type::Void,
            });
            None
        } else {
            let local = self.new_temporary(return_type.clone());
            let destination = mir::Place::Local(local);
            self.instruction(mir::Instruction::Call {
                destination: Some(destination.clone()),
                function,
                arguments: lowered_arguments,
                return_type: return_type.clone(),
            });
            Some(mir::Operand {
                type_: return_type.clone(),
                kind: mir::OperandKind::Copy(destination),
            })
        }
    }
}
