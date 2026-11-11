//! MEM-EXP-1 C5/C6 ownership pass (master plan §5.5, closure §4.B).
//!
//! Experiment only: the default build (C0) never calls it. It rewrites a
//! validated module so that every owning reference slot accounts for exactly
//! one strong count, using explicit [`crate::Instruction::Retain`] and
//! [`crate::Instruction::Release`]. Its output is checked by
//! [`crate::validate::ownership`], which shares no code with this pass.

use std::collections::HashSet;

use crate as mir;
use mir::{
    BasicBlock, BasicBlockId, Function, Instruction, Local, LocalId, Module, Operand, OperandKind,
    Place, Rvalue, RvalueKind, SymbolId, Terminator, Type,
};

/// Count operations inserted by [`insert_ownership`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnershipStats {
    pub retains: usize,
    pub releases: usize,
}

/// Return a copy of `module` with ownership operations inserted.
#[must_use]
pub fn insert_ownership(module: &mir::Module) -> (mir::Module, OwnershipStats) {
    let mut stats = OwnershipStats::default();
    let mut output = module.clone();
    let native = string_builder_classes(module);
    for function in &mut output.functions {
        let original = function.clone();
        let mut context = Context {
            module,
            native: &native,
            function: &original,
            next_local: next_id(
                function
                    .parameters
                    .iter()
                    .chain(&function.locals)
                    .map(|l| l.id.0),
            ),
            new_locals: Vec::new(),
            stats: &mut stats,
        };
        context.rewrite_function(function);
    }
    (output, stats)
}

fn next_id(ids: impl Iterator<Item = u32>) -> u32 {
    ids.max().map_or(0, |id| id + 1)
}

type Leaf = (Vec<SymbolId>, Type);

/// `StringBuilder` objects use a native runtime header without a count word.
fn string_builder_classes(module: &Module) -> HashSet<SymbolId> {
    module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            Instruction::AllocateStringBuilder { class, .. }
            | Instruction::StringBuilderAppend { class, .. }
            | Instruction::StringBuilderToString { class, .. } => Some(*class),
            _ => None,
        })
        .collect()
}

fn leaves(module: &Module, native: &HashSet<SymbolId>, type_: &Type) -> Vec<Leaf> {
    match type_ {
        Type::Class(symbol) if native.contains(symbol) => Vec::new(),
        Type::Class(_) | Type::Interface(_) => vec![(Vec::new(), type_.clone())],
        Type::User(symbol) => module
            .structs
            .iter()
            .find(|definition| definition.symbol == *symbol)
            .into_iter()
            .flat_map(|definition| &definition.fields)
            .flat_map(|field| {
                leaves(module, native, &field.type_)
                    .into_iter()
                    .map(move |(mut path, leaf)| {
                        path.insert(0, field.symbol);
                        (path, leaf)
                    })
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn leaf_place(base: &Place, path: &[SymbolId]) -> Place {
    path.iter().fold(base.clone(), |base, field| Place::Field {
        base: Box::new(base),
        field: *field,
    })
}

fn copy(place: Place, type_: &Type) -> Operand {
    Operand {
        type_: type_.clone(),
        kind: OperandKind::Copy(place),
    }
}

fn owning(place: &Place) -> bool {
    match place {
        Place::Local(_) | Place::ObjectField { .. } => true,
        Place::Field { base, .. } => owning(base),
        Place::Symbol(_) | Place::Index { .. } | Place::EnumField { .. } => false,
    }
}

struct Context<'a> {
    module: &'a Module,
    native: &'a HashSet<SymbolId>,
    function: &'a Function,
    next_local: u32,
    new_locals: Vec<Local>,
    stats: &'a mut OwnershipStats,
}

impl Context<'_> {
    fn leaf_operands(&self, place: &Place, type_: &Type) -> Vec<Operand> {
        leaves(self.module, self.native, type_)
            .into_iter()
            .map(|(path, leaf)| copy(leaf_place(place, &path), &leaf))
            .collect()
    }

    fn operand_leaves(&self, operand: &Operand) -> Vec<Operand> {
        match &operand.kind {
            OperandKind::Copy(place) => self.leaf_operands(place, &operand.type_),
            OperandKind::Constant(_) | OperandKind::Function(_) => Vec::new(),
        }
    }

    fn place_type(&self, place: &Place) -> Option<Type> {
        match place {
            Place::Local(id) => self
                .function
                .parameters
                .iter()
                .chain(&self.function.locals)
                .find(|local| local.id == *id)
                .map(|local| local.type_.clone()),
            Place::Symbol(_) => None,
            Place::Field { base, field } => {
                let Type::User(symbol) = self.place_type(base)? else {
                    return None;
                };
                let definition = self.module.structs.iter().find(|d| d.symbol == symbol)?;
                definition
                    .fields
                    .iter()
                    .find(|f| f.symbol == *field)
                    .map(|f| f.type_.clone())
            }
            Place::Index { element_type, .. } => Some(element_type.clone()),
            Place::ObjectField { object, field } => {
                let Type::Class(symbol) = &object.type_ else {
                    return None;
                };
                let definition = self.module.classes.iter().find(|d| d.symbol == *symbol)?;
                definition
                    .fields
                    .iter()
                    .find(|f| f.symbol == *field)
                    .map(|f| f.type_.clone())
            }
            Place::EnumField { case, field, .. } => self
                .module
                .enums
                .iter()
                .flat_map(|definition| &definition.cases)
                .find(|candidate| candidate.symbol == *case)?
                .fields
                .iter()
                .find(|f| f.symbol == *field)
                .map(|f| f.type_.clone()),
        }
    }

    fn fresh(&mut self, type_: &Type, name: &str) -> LocalId {
        let id = LocalId(self.next_local);
        self.next_local += 1;
        self.new_locals.push(Local {
            id,
            symbol: None,
            name: format!("{name}.{}", id.0),
            type_: type_.clone(),
            mutable: true,
            temporary: true,
        });
        id
    }

    fn retain(&mut self, out: &mut Vec<Instruction>, value: Operand) {
        self.stats.retains += 1;
        out.push(Instruction::Retain { value });
    }

    fn release(&mut self, out: &mut Vec<Instruction>, value: Operand) {
        self.stats.releases += 1;
        out.push(Instruction::Release { value });
    }

    fn owning_destinations(&self, place: &Place) -> Vec<Operand> {
        if !owning(place) {
            return Vec::new();
        }
        self.place_type(place)
            .map(|type_| self.leaf_operands(place, &type_))
            .unwrap_or_default()
    }

    fn consumed_and_destinations(
        &self,
        instruction: &Instruction,
    ) -> (Vec<Operand>, Vec<Operand>, bool) {
        let mut sources: Vec<&Operand> = Vec::new();
        let mut destination: Option<&Place> = None;
        let mut borrowed_result = false;
        match instruction {
            Instruction::Assign { target, value } => {
                destination = Some(target);
                match &value.kind {
                    RvalueKind::Use(operand)
                    | RvalueKind::Cast(operand)
                    | RvalueKind::MakeInterface {
                        object: operand, ..
                    } => sources.push(operand),
                    RvalueKind::Aggregate(fields) | RvalueKind::EnumConstruct { fields, .. } => {
                        sources.extend(fields.iter().map(|field| &field.value));
                    }
                    _ => {}
                }
            }
            Instruction::Call { destination: d, .. }
            | Instruction::CallInterface { destination: d, .. } => destination = d.as_ref(),
            Instruction::ForeignCall {
                destination: d,
                arguments,
                ..
            } => {
                destination = d.as_ref();
                sources.extend(arguments);
            }
            Instruction::CallIntrinsic {
                destination: d,
                arguments,
                ..
            } => {
                destination = d.as_ref();
                borrowed_result = true;
                sources.extend(arguments);
            }
            Instruction::AllocateObject { destination: d, .. } => destination = Some(d),
            Instruction::ListAdd { value, .. } | Instruction::ListSet { value, .. } => {
                sources.push(value);
            }
            Instruction::ListGet { destination: d, .. }
            | Instruction::DictionaryTryGet { destination: d, .. } => {
                destination = Some(d);
                borrowed_result = true;
            }
            Instruction::DictionaryAdd { key, value, .. }
            | Instruction::DictionarySet { key, value, .. } => sources.extend([key, value]),
            _ => {}
        }
        let consumed = sources
            .into_iter()
            .flat_map(|operand| self.operand_leaves(operand))
            .collect();
        let destinations = destination
            .map(|place| self.owning_destinations(place))
            .unwrap_or_default();
        (consumed, destinations, borrowed_result)
    }

    fn rewrite_instruction(&mut self, instruction: Instruction, out: &mut Vec<Instruction>) {
        // Region rewinds in the middle of a function would leave counted
        // locals pointing into reclaimed memory; removal only stops reclamation.
        if matches!(
            instruction,
            Instruction::OwnedRegionEnter { .. }
                | Instruction::OwnedRegionExit { .. }
                | Instruction::TemporarySubregionEnter { .. }
                | Instruction::TemporarySubregionExit { .. }
        ) {
            return;
        }
        let (consumed, destinations, borrowed_result) =
            self.consumed_and_destinations(&instruction);
        for operand in consumed {
            self.retain(out, operand);
        }
        let mut captured = Vec::new();
        for destination in &destinations {
            let temporary = self.fresh(&destination.type_, "$own.old");
            captured.push(copy(Place::Local(temporary), &destination.type_));
            out.push(Instruction::Assign {
                target: Place::Local(temporary),
                value: Rvalue {
                    type_: destination.type_.clone(),
                    kind: RvalueKind::Use(destination.clone()),
                },
            });
        }
        out.push(instruction);
        if borrowed_result {
            for destination in destinations {
                self.retain(out, destination);
            }
        }
        for old in captured {
            self.release(out, old);
        }
    }

    fn release_slots(&mut self, out: &mut Vec<Instruction>) {
        // `self.function` is the pre-pass function, so pass temporaries never appear here.
        let function = self.function;
        for local in function.parameters.iter().chain(&function.locals) {
            for slot in self.leaf_operands(&Place::Local(local.id), &local.type_) {
                self.release(out, slot);
            }
        }
    }

    fn rewrite_terminator(
        &mut self,
        terminator: Terminator,
        out: &mut Vec<Instruction>,
    ) -> Terminator {
        match terminator {
            // Any copied operand is read before the exit releases: even a
            // scalar such as `this.value` reads through a reference.
            Terminator::Return(Some(operand))
                if matches!(operand.kind, OperandKind::Copy(_))
                    || !leaves(self.module, self.native, &operand.type_).is_empty() =>
            {
                let result = self.fresh(&operand.type_, "$own.ret");
                out.push(Instruction::Assign {
                    target: Place::Local(result),
                    value: Rvalue {
                        type_: operand.type_.clone(),
                        kind: RvalueKind::Use(operand.clone()),
                    },
                });
                for leaf in self.leaf_operands(&Place::Local(result), &operand.type_) {
                    self.retain(out, leaf);
                }
                self.release_slots(out);
                Terminator::Return(Some(copy(Place::Local(result), &operand.type_)))
            }
            terminator @ (Terminator::Return(_) | Terminator::End) => {
                self.release_slots(out);
                terminator
            }
            other => other,
        }
    }

    fn rewrite_function(&mut self, function: &mut Function) {
        let blocks = std::mem::take(&mut function.blocks);
        function.blocks = blocks
            .into_iter()
            .map(|block| {
                let mut instructions = Vec::new();
                for instruction in block.instructions {
                    self.rewrite_instruction(instruction, &mut instructions);
                }
                let terminator = self.rewrite_terminator(block.terminator, &mut instructions);
                BasicBlock {
                    id: block.id,
                    instructions,
                    terminator,
                }
            })
            .collect();
        let parameter_slots: Vec<Operand> = function
            .parameters
            .iter()
            .flat_map(|parameter| self.leaf_operands(&Place::Local(parameter.id), &parameter.type_))
            .collect();
        if !parameter_slots.is_empty() {
            let id = BasicBlockId(next_id(function.blocks.iter().map(|block| block.id.0)));
            let mut instructions = Vec::new();
            for slot in parameter_slots {
                self.retain(&mut instructions, slot);
            }
            function.blocks.insert(
                0,
                BasicBlock {
                    id,
                    instructions,
                    terminator: Terminator::Goto(function.entry),
                },
            );
            function.entry = id;
        }
        function.locals.append(&mut self.new_locals);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validate::ownership::validate_ownership;
    use mir::{FieldDefinition, Visibility};

    fn class() -> Type {
        Type::Class(SymbolId(10))
    }

    fn iface() -> Type {
        Type::Interface(SymbolId(30))
    }

    fn struct_type() -> Type {
        Type::User(SymbolId(20))
    }

    fn local(id: u32, type_: Type, temporary: bool) -> Local {
        Local {
            id: LocalId(id),
            symbol: None,
            name: format!("l{id}"),
            type_,
            mutable: true,
            temporary,
        }
    }

    fn lp(id: u32) -> Place {
        Place::Local(LocalId(id))
    }

    fn field(id: u32) -> Place {
        Place::ObjectField {
            object: Box::new(copy(lp(id), &class())),
            field: SymbolId(11),
        }
    }

    fn assign(target: Place, type_: &Type, source: Operand) -> Instruction {
        Instruction::Assign {
            target,
            value: Rvalue {
                type_: type_.clone(),
                kind: RvalueKind::Use(source),
            },
        }
    }

    fn retain(place: Place, type_: &Type) -> Instruction {
        Instruction::Retain {
            value: copy(place, type_),
        }
    }

    fn release(place: Place, type_: &Type) -> Instruction {
        Instruction::Release {
            value: copy(place, type_),
        }
    }

    fn field_definition(symbol: u32, type_: Type) -> FieldDefinition {
        FieldDefinition {
            symbol: SymbolId(symbol),
            name: format!("f{symbol}"),
            type_,
        }
    }

    fn run(
        parameters: Vec<Local>,
        locals: Vec<Local>,
        instructions: Vec<Instruction>,
        terminator: Terminator,
        return_type: Type,
    ) -> (Function, OwnershipStats) {
        let module = Module {
            structs: vec![mir::StructDefinition {
                symbol: SymbolId(20),
                name: "S".into(),
                fields: vec![field_definition(21, class())],
            }],
            classes: vec![mir::ClassDefinition {
                symbol: SymbolId(10),
                name: "C".into(),
                fields: vec![field_definition(11, class())],
            }],
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: vec![Function {
                constructor: false,
                symbol: SymbolId(1),
                owner: None,
                name: "f".into(),
                visibility: Visibility::Public,
                parameters,
                locals,
                return_type,
                entry: BasicBlockId(0),
                blocks: vec![BasicBlock {
                    id: BasicBlockId(0),
                    instructions,
                    terminator,
                }],
                temporary_subregion_candidates: Vec::new(),
            }],
        };
        let (output, stats) = insert_ownership(&module);
        assert_eq!(validate_ownership(&output), Ok(()), "{output:#?}");
        (output.functions.into_iter().next().unwrap(), stats)
    }

    fn run_void(
        parameters: Vec<Local>,
        locals: Vec<Local>,
        instructions: Vec<Instruction>,
    ) -> (Function, OwnershipStats) {
        run(
            parameters,
            locals,
            instructions,
            Terminator::Return(None),
            Type::Void,
        )
    }

    #[test]
    fn local_assignment_captures_and_releases_old_value() {
        let (function, stats) = run_void(
            vec![local(0, class(), false)],
            vec![local(1, class(), false)],
            vec![assign(lp(1), &class(), copy(lp(0), &class()))],
        );
        assert_eq!(function.entry, BasicBlockId(1));
        assert_eq!(
            function.blocks[0].instructions,
            vec![retain(lp(0), &class())]
        );
        assert_eq!(
            function.blocks[1].instructions,
            vec![
                retain(lp(0), &class()),
                assign(lp(2), &class(), copy(lp(1), &class())),
                assign(lp(1), &class(), copy(lp(0), &class())),
                release(lp(2), &class()),
                release(lp(0), &class()),
                release(lp(1), &class()),
            ]
        );
        assert!(function.locals[1].temporary);
        assert_eq!(
            stats,
            OwnershipStats {
                retains: 2,
                releases: 3
            }
        );
    }

    #[test]
    fn self_assignment_of_field_retains_before_capture() {
        let (function, _) = run_void(
            vec![local(0, class(), false)],
            Vec::new(),
            vec![assign(field(0), &class(), copy(field(0), &class()))],
        );
        assert_eq!(
            function.blocks[1].instructions,
            vec![
                retain(field(0), &class()),
                assign(lp(1), &class(), copy(field(0), &class())),
                assign(field(0), &class(), copy(field(0), &class())),
                release(lp(1), &class()),
                release(lp(0), &class()),
            ]
        );
    }

    #[test]
    fn field_alias_assignment_validates() {
        run_void(
            vec![local(0, class(), false), local(1, class(), false)],
            Vec::new(),
            vec![assign(field(0), &class(), copy(field(1), &class()))],
        );
    }

    #[test]
    fn field_overwrite_with_call_result_validates() {
        let (function, _) = run_void(
            vec![local(0, class(), false)],
            Vec::new(),
            vec![Instruction::Call {
                destination: Some(field(0)),
                function: SymbolId(2),
                arguments: Vec::new(),
                return_type: class(),
            }],
        );
        let retains = function.blocks[1]
            .instructions
            .iter()
            .filter(|i| matches!(i, Instruction::Retain { .. }))
            .count();
        assert_eq!(retains, 0);
    }

    #[test]
    fn returning_a_parameter_retains_the_result_local() {
        run(
            vec![local(0, class(), false)],
            Vec::new(),
            Vec::new(),
            Terminator::Return(Some(copy(lp(0), &class()))),
            class(),
        );
    }

    #[test]
    fn returning_a_field_load_validates() {
        run(
            vec![local(0, class(), false)],
            Vec::new(),
            Vec::new(),
            Terminator::Return(Some(copy(field(0), &class()))),
            class(),
        );
    }

    #[test]
    fn struct_local_with_class_field_copy_validates() {
        let (function, _) = run_void(
            vec![local(0, struct_type(), false)],
            vec![local(1, struct_type(), false)],
            vec![assign(lp(1), &struct_type(), copy(lp(0), &struct_type()))],
        );
        let leaf = Place::Field {
            base: Box::new(lp(0)),
            field: SymbolId(21),
        };
        assert_eq!(
            function.blocks[0].instructions,
            vec![retain(leaf, &class())]
        );
    }

    #[test]
    fn interface_value_from_make_interface_validates() {
        run(
            vec![local(0, class(), false)],
            vec![local(1, iface(), false)],
            vec![Instruction::Assign {
                target: lp(1),
                value: Rvalue {
                    type_: iface(),
                    kind: RvalueKind::MakeInterface {
                        object: copy(lp(0), &class()),
                        class: SymbolId(10),
                        interface: SymbolId(30),
                    },
                },
            }],
            Terminator::Return(Some(copy(lp(1), &iface()))),
            iface(),
        );
    }

    #[test]
    fn list_add_of_class_value_consumes_a_retain() {
        let list = Type::List(Box::new(class()));
        let (function, _) = run_void(
            vec![local(0, list.clone(), false), local(1, class(), false)],
            Vec::new(),
            vec![Instruction::ListAdd {
                list: copy(lp(0), &list),
                value: copy(lp(1), &class()),
            }],
        );
        assert_eq!(function.blocks[1].instructions[0], retain(lp(1), &class()));
    }

    #[test]
    fn call_argument_object_field_load_is_borrowed() {
        let (function, _) = run_void(
            vec![local(0, class(), false)],
            Vec::new(),
            vec![Instruction::Call {
                destination: None,
                function: SymbolId(2),
                arguments: vec![copy(field(0), &class())],
                return_type: Type::Void,
            }],
        );
        assert_eq!(function.blocks[1].instructions.len(), 2);
    }
}
