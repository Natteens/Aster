//! Independent validator for the MEM-EXP-1 ownership discipline
//! (obligations U1, U2, U4 of master plan §5.6). Shares no code with
//! [`crate::ownership`].

use std::collections::HashMap;
use std::fmt;

use crate as mir;
use mir::{
    BasicBlock, BasicBlockId, Function, LocalId, Module, Operand, OperandKind, Place, SymbolId,
    Type,
};

/// One ownership obligation the module violates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnershipFinding {
    pub function: String,
    pub obligation: &'static str,
    pub message: String,
}

impl fmt::Display for OwnershipFinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} in `{}`: {}",
            self.obligation, self.function, self.message
        )
    }
}

/// Check every function of an ownership-annotated module.
///
/// # Errors
///
/// Returns every finding, in deterministic function and block order.
pub fn validate_ownership(module: &mir::Module) -> Result<(), Vec<OwnershipFinding>> {
    let mut findings = Vec::new();
    let native = native_header_classes(module);
    for function in &module.functions {
        check_function(module, &native, function, &mut findings);
    }
    if findings.is_empty() {
        Ok(())
    } else {
        Err(findings)
    }
}

/// Classes whose objects carry a runtime-native header (no count word).
fn native_header_classes(module: &Module) -> Vec<SymbolId> {
    let mut native = Vec::new();
    for instruction in module
        .functions
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.instructions)
    {
        if let mir::Instruction::AllocateStringBuilder { class, .. }
        | mir::Instruction::StringBuilderAppend { class, .. }
        | mir::Instruction::StringBuilderToString { class, .. } = instruction
            && !native.contains(class)
        {
            native.push(*class);
        }
    }
    native
}

type Path = Vec<SymbolId>;
type ValueId = usize;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Null,
    Live,
    Dead,
}

impl SlotState {
    fn join(self, other: Self) -> Self {
        match (self, other) {
            (a, b) if a == b => a,
            (Self::Dead, _) | (_, Self::Dead) => Self::Dead,
            _ => Self::Live,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Provenance {
    Slot,
    Owned,
    HeapRead,
    Guaranteed,
    /// A parameter's original value, before any count of this function.
    Param,
    Region,
    Null,
}

struct Value {
    tokens: i64,
    provenance: Provenance,
    epoch: u32,
    persistent: bool,
}

enum Home {
    Slot(usize),
    Heap,
    Outside,
}

#[derive(Clone, Copy)]
enum Product {
    Owned,
    Borrowed,
    Allocation { temporary: bool },
}

struct Write {
    place: Place,
    value: ValueId,
    borrowed: bool,
}

struct Frame<'a> {
    module: &'a Module,
    native: &'a [SymbolId],
    function: &'a Function,
    keys: Vec<(LocalId, Path)>,
    /// C6 leaf-function mode: parameters are held without a count.
    borrowed_params: bool,
}

fn extend(place: &Place, path: &[SymbolId]) -> Place {
    let mut result = place.clone();
    for field in path {
        result = Place::Field {
            base: Box::new(result),
            field: *field,
        };
    }
    result
}

fn mentions(place: &Place, local: LocalId) -> bool {
    let in_operand = |operand: &Operand| match &operand.kind {
        OperandKind::Copy(inner) => mentions(inner, local),
        _ => false,
    };
    match place {
        Place::Local(id) => *id == local,
        Place::Symbol(_) => false,
        Place::Field { base, .. } | Place::EnumField { base, .. } => mentions(base, local),
        Place::ObjectField { object, .. } => in_operand(object),
        Place::Index { array, index, .. } => in_operand(array) || in_operand(index),
    }
}

impl Frame<'_> {
    fn leaf_types(&self, type_: &Type) -> Vec<(Path, Type)> {
        let mut found = Vec::new();
        let mut pending = vec![(Vec::new(), type_.clone())];
        while let Some((path, current)) = pending.pop() {
            match current {
                Type::Class(symbol) if self.native.contains(&symbol) => {}
                Type::Class(_) | Type::Interface(_) => found.push((path, current)),
                Type::User(symbol) => {
                    if let Some(definition) =
                        self.module.structs.iter().find(|d| d.symbol == symbol)
                    {
                        for field in definition.fields.iter().rev() {
                            let mut inner = path.clone();
                            inner.push(field.symbol);
                            pending.push((inner, field.type_.clone()));
                        }
                    }
                }
                _ => {}
            }
        }
        found
    }

    fn leaf_places(&self, place: &Place, type_: &Type) -> Vec<(Path, Place)> {
        self.leaf_types(type_)
            .into_iter()
            .map(|(path, _)| {
                let leaf = extend(place, &path);
                (path, leaf)
            })
            .collect()
    }

    fn place_type(&self, place: &Place) -> Option<Type> {
        match place {
            Place::Local(id) => {
                let function = self.function;
                function
                    .parameters
                    .iter()
                    .chain(&function.locals)
                    .find(|local| local.id == *id)
                    .map(|local| local.type_.clone())
            }
            Place::Symbol(_) => None,
            Place::Field { base, field } => match self.place_type(base)? {
                Type::User(symbol) => {
                    let definition = self.module.structs.iter().find(|d| d.symbol == symbol)?;
                    definition
                        .fields
                        .iter()
                        .find(|f| f.symbol == *field)
                        .map(|f| f.type_.clone())
                }
                _ => None,
            },
            Place::Index { element_type, .. } => Some(element_type.clone()),
            Place::ObjectField { object, field } => match &object.type_ {
                Type::Class(symbol) => {
                    let definition = self.module.classes.iter().find(|d| d.symbol == *symbol)?;
                    definition
                        .fields
                        .iter()
                        .find(|f| f.symbol == *field)
                        .map(|f| f.type_.clone())
                }
                _ => None,
            },
            Place::EnumField { case, field, .. } => {
                let found = self
                    .module
                    .enums
                    .iter()
                    .flat_map(|e| &e.cases)
                    .find(|c| c.symbol == *case)?;
                found
                    .fields
                    .iter()
                    .find(|f| f.symbol == *field)
                    .map(|f| f.type_.clone())
            }
        }
    }

    fn home(&self, place: &Place) -> Home {
        let mut path = Vec::new();
        let mut current = place;
        while let Place::Field { base, field } = current {
            path.push(*field);
            current = base;
        }
        path.reverse();
        match current {
            Place::Local(id) => self
                .keys
                .iter()
                .position(|(local, slot_path)| local == id && *slot_path == path)
                .map_or(Home::Outside, Home::Slot),
            Place::ObjectField { .. } => Home::Heap,
            _ => Home::Outside,
        }
    }

    fn is_parameter_slot(&self, index: usize) -> bool {
        let local = self.keys[index].0;
        self.function.parameters.iter().any(|p| p.id == local)
    }
}

/// Backward slot liveness of one block: slots read (including by a release)
/// before being redefined, and slots redefined.
struct Liveness {
    live_in: Vec<bool>,
    live_out: Vec<bool>,
}

struct Block<'a> {
    frame: &'a Frame<'a>,
    id: BasicBlockId,
    slots: Vec<SlotState>,
    held: Vec<Option<ValueId>>,
    values: Vec<Value>,
    cache: Vec<(Place, ValueId)>,
    epoch: u32,
    findings: Vec<(&'static str, String)>,
    /// Slots read before any redefinition in this block.
    used: Vec<bool>,
    /// Slots redefined in this block.
    defined: Vec<bool>,
    /// Slots that are live at the block end; `None` counts every slot.
    live_out: Option<Vec<bool>>,
}

impl<'a> Block<'a> {
    fn run(
        frame: &'a Frame<'a>,
        block: &BasicBlock,
        entry: &[SlotState],
        first: bool,
        liveness: Option<&Liveness>,
    ) -> Self {
        let mut state = Self {
            frame,
            id: block.id,
            slots: entry.to_vec(),
            held: vec![None; entry.len()],
            values: Vec::new(),
            cache: Vec::new(),
            epoch: 0,
            findings: Vec::new(),
            used: vec![false; entry.len()],
            defined: vec![false; entry.len()],
            live_out: liveness.map(|l| l.live_out.clone()),
        };
        for (index, slot) in entry.iter().enumerate() {
            if *slot == SlotState::Live {
                let value = if first || (frame.borrowed_params && frame.is_parameter_slot(index)) {
                    state.value(0, Provenance::Param)
                } else if liveness.is_none_or(|l| l.live_in[index]) {
                    state.value(1, Provenance::Slot)
                } else {
                    // The bits are never read again: no count is attributed.
                    state.value(0, Provenance::Slot)
                };
                state.held[index] = Some(value);
            }
        }
        for instruction in &block.instructions {
            state.step(instruction);
        }
        state.finish(&block.terminator);
        state
    }

    fn flag(&mut self, obligation: &'static str, message: &str) {
        self.findings
            .push((obligation, format!("block {}: {message}", self.id.0)));
    }

    fn value(&mut self, tokens: i64, provenance: Provenance) -> ValueId {
        self.values.push(Value {
            tokens,
            provenance,
            epoch: self.epoch,
            persistent: false,
        });
        self.values.len() - 1
    }

    fn counted(&self, value: ValueId) -> bool {
        !matches!(
            self.values[value].provenance,
            Provenance::Region | Provenance::Null
        )
    }

    fn adjust(&mut self, value: ValueId, delta: i64, allow_negative: bool) {
        if !self.counted(value) {
            return;
        }
        self.values[value].tokens += delta;
        if self.values[value].tokens < 0 && !allow_negative {
            self.flag("U1", "a count is consumed that the value does not hold");
        }
    }

    fn mark_used(&mut self, index: usize) {
        if !self.defined[index] {
            self.used[index] = true;
        }
    }

    fn invalidate(&mut self) {
        self.cache.clear();
        self.epoch += 1;
    }

    fn check_stale(&mut self, value: ValueId) {
        let entry = &self.values[value];
        let borrowed = match entry.provenance {
            Provenance::HeapRead => true,
            Provenance::Param => !self.frame.borrowed_params,
            _ => false,
        };
        if borrowed && entry.tokens <= 0 && entry.epoch < self.epoch {
            self.flag(
                "U2",
                "borrowed heap value used after an invalidating instruction",
            );
        }
    }

    fn read_leaf(&mut self, place: &Place) -> ValueId {
        match self.frame.home(place) {
            Home::Slot(index) => {
                self.mark_used(index);
                match (self.slots[index], self.held[index]) {
                    (SlotState::Live, Some(value)) => {
                        self.check_stale(value);
                        value
                    }
                    (SlotState::Dead, _) => {
                        self.flag("U2", "use of a slot after its release");
                        self.value(0, Provenance::Null)
                    }
                    _ => self.value(0, Provenance::Null),
                }
            }
            home @ (Home::Heap | Home::Outside) => {
                if let Some((_, value)) = self.cache.iter().find(|(cached, _)| cached == place) {
                    return *value;
                }
                // List element reads are `HeapRead` (ListGet products); array
                // (`Index`) reads stay Guaranteed because arrays still leak (§1).
                let provenance = if matches!(home, Home::Heap) {
                    Provenance::HeapRead
                } else {
                    Provenance::Guaranteed
                };
                let value = self.value(0, provenance);
                self.cache.push((place.clone(), value));
                value
            }
        }
    }

    fn touch_place(&mut self, place: &Place) {
        match place {
            Place::Field { base, .. } | Place::EnumField { base, .. } => self.touch_place(base),
            Place::ObjectField { object, .. } => {
                self.operand_values(object);
            }
            Place::Index { array, index, .. } => {
                self.operand_values(array);
                self.operand_values(index);
            }
            Place::Local(_) | Place::Symbol(_) => {}
        }
    }

    fn operand_values(&mut self, operand: &Operand) -> Vec<(Path, ValueId)> {
        let OperandKind::Copy(place) = &operand.kind else {
            return Vec::new();
        };
        self.touch_place(place);
        let leaves = self.frame.leaf_places(place, &operand.type_);
        leaves
            .into_iter()
            .map(|(path, leaf)| (path, self.read_leaf(&leaf)))
            .collect()
    }

    fn retain(&mut self, operand: &Operand) {
        for (_, value) in self.operand_values(operand) {
            self.adjust(value, 1, false);
        }
    }

    fn release(&mut self, operand: &Operand) {
        let OperandKind::Copy(place) = &operand.kind else {
            return;
        };
        self.touch_place(place);
        for (_, leaf) in self.frame.leaf_places(place, &operand.type_) {
            let slot = match self.frame.home(&leaf) {
                Home::Slot(index) => Some(index),
                _ => None,
            };
            if let Some(index) = slot {
                self.mark_used(index);
            }
            let value = match slot {
                Some(index) => match self.slots[index] {
                    SlotState::Dead => {
                        self.flag("U1", "release of a slot that was already released");
                        None
                    }
                    SlotState::Null => None,
                    SlotState::Live => self.held[index],
                },
                None => Some(self.read_leaf(&leaf)),
            };
            if let Some(value) = value {
                if self.counted(value) && self.values[value].tokens <= 0 {
                    self.flag("U1", "release of a value that holds no count");
                } else {
                    self.adjust(value, -1, false);
                }
            }
            if let Some(index) = slot {
                self.slots[index] = SlotState::Dead;
                self.held[index] = None;
            }
        }
        self.invalidate();
    }

    fn produce(&mut self, product: Product) -> (ValueId, bool) {
        match product {
            Product::Owned => (self.value(1, Provenance::Owned), false),
            Product::Borrowed => (self.value(0, Provenance::HeapRead), true),
            Product::Allocation { temporary: true } => (self.value(0, Provenance::Region), false),
            Product::Allocation { temporary: false } => {
                let value = self.value(1, Provenance::Owned);
                self.values[value].persistent = true;
                (value, false)
            }
        }
    }

    fn root_object(place: &Place) -> Option<&Operand> {
        match place {
            Place::Field { base, .. } => Self::root_object(base),
            Place::ObjectField { object, .. } => Some(object),
            _ => None,
        }
    }

    fn write_all(&mut self, writes: Vec<Write>, clobber: bool) {
        let olds: Vec<Option<ValueId>> = writes
            .iter()
            .map(|write| match self.frame.home(&write.place) {
                Home::Heap => self
                    .cache
                    .iter()
                    .find(|(p, _)| *p == write.place)
                    .map(|(_, v)| *v),
                _ => None,
            })
            .collect();
        if clobber {
            self.invalidate();
        }
        for (write, old) in writes.into_iter().zip(olds) {
            match self.frame.home(&write.place) {
                Home::Slot(index) => {
                    let null = self.values[write.value].provenance == Provenance::Null;
                    self.slots[index] = if null {
                        SlotState::Null
                    } else {
                        SlotState::Live
                    };
                    self.held[index] = Some(write.value);
                    self.defined[index] = true;
                    let local = self.frame.keys[index].0;
                    self.cache.retain(|(place, _)| !mentions(place, local));
                }
                Home::Heap => {
                    match old {
                        Some(old) => self.adjust(old, 1, false),
                        None => self.flag(
                            "U1",
                            "store into an owning field whose old value was not read",
                        ),
                    }
                    self.adjust(write.value, -1, write.borrowed);
                    if self.values[write.value].provenance == Provenance::Region {
                        let base = Self::root_object(&write.place)
                            .map(|object| self.operand_values(object))
                            .and_then(|values| values.first().map(|(_, v)| *v));
                        if base.is_some_and(|base| self.values[base].persistent) {
                            self.flag("U4", "Temporary allocation stored into a Persistent object");
                        }
                    }
                    self.invalidate();
                    self.cache.push((write.place, write.value));
                }
                Home::Outside => {
                    if !write.borrowed {
                        self.adjust(write.value, -1, false);
                    }
                    self.invalidate();
                }
            }
        }
    }

    fn effect(
        &mut self,
        plain: &[&Operand],
        consumed: &[&Operand],
        destination: Option<&Place>,
        product: Product,
        clobber: bool,
    ) {
        for operand in plain {
            self.operand_values(operand);
        }
        for operand in consumed {
            for (_, value) in self.operand_values(operand) {
                self.adjust(value, -1, false);
            }
        }
        let mut writes = Vec::new();
        if let Some(place) = destination {
            self.touch_place(place);
            if let Some(type_) = self.frame.place_type(place) {
                for (_, leaf) in self.frame.leaf_places(place, &type_) {
                    let (value, borrowed) = self.produce(product);
                    writes.push(Write {
                        place: leaf,
                        value,
                        borrowed,
                    });
                }
            }
        }
        if !writes.is_empty() || clobber {
            self.write_all(writes, clobber);
        }
    }

    fn assign(&mut self, target: &Place, value: &mir::Rvalue) {
        use mir::RvalueKind as K;
        let mut sources: Vec<(Path, ValueId)> = Vec::new();
        match &value.kind {
            K::Use(o) | K::Cast(o) | K::MakeInterface { object: o, .. } => {
                sources = self.operand_values(o);
            }
            K::Aggregate(fields) => {
                for field in fields {
                    for (mut path, found) in self.operand_values(&field.value) {
                        path.insert(0, field.field);
                        sources.push((path, found));
                    }
                }
            }
            K::EnumConstruct { fields, .. } => {
                for field in fields {
                    for (_, found) in self.operand_values(&field.value) {
                        self.adjust(found, -1, false);
                    }
                }
            }
            K::Discriminant(o)
            | K::ArrayLength(o)
            | K::ListLength(o)
            | K::DictionaryLength(o)
            | K::ListVersion(o)
            | K::StringByteLength(o)
            | K::Unary { operand: o, .. } => {
                self.operand_values(o);
            }
            K::Binary { left, right, .. } | K::Equality { left, right, .. } => {
                self.operand_values(left);
                self.operand_values(right);
            }
        }
        self.touch_place(target);
        let mut writes = Vec::new();
        for (path, leaf) in self.frame.leaf_places(target, &value.type_) {
            let found = match sources.iter().find(|(p, _)| *p == path) {
                Some((_, found)) => *found,
                None => self.value(0, Provenance::Null),
            };
            writes.push(Write {
                place: leaf,
                value: found,
                borrowed: false,
            });
        }
        if !writes.is_empty() {
            self.write_all(writes, false);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn step(&mut self, instruction: &mir::Instruction) {
        use mir::Instruction as I;
        match instruction {
            I::Retain { value } => self.retain(value),
            I::Release { value } => self.release(value),
            I::Assign { target, value } => self.assign(target, value),
            I::Call {
                destination,
                arguments,
                ..
            } => {
                let plain: Vec<&Operand> = arguments.iter().collect();
                self.effect(&plain, &[], destination.as_ref(), Product::Owned, true);
            }
            I::CallInterface {
                destination,
                receiver,
                arguments,
                ..
            } => {
                let plain: Vec<&Operand> = std::iter::once(receiver).chain(arguments).collect();
                self.effect(&plain, &[], destination.as_ref(), Product::Owned, true);
            }
            I::ForeignCall {
                destination,
                arguments,
                ..
            } => {
                let consumed: Vec<&Operand> = arguments.iter().collect();
                self.effect(&[], &consumed, destination.as_ref(), Product::Owned, true);
            }
            I::CallIntrinsic {
                destination,
                arguments,
                ..
            } => {
                let consumed: Vec<&Operand> = arguments.iter().collect();
                self.effect(
                    &[],
                    &consumed,
                    destination.as_ref(),
                    Product::Borrowed,
                    true,
                );
            }
            I::AllocateObject {
                destination,
                region,
                ..
            } => {
                let temporary = *region == mir::AllocationRegion::Temporary;
                self.effect(
                    &[],
                    &[],
                    Some(destination),
                    Product::Allocation { temporary },
                    false,
                );
            }
            I::ListAdd { list, value } => {
                self.effect(&[list], &[value], None, Product::Owned, false);
            }
            I::ListSet { list, index, value } => {
                // The runtime releases the old element: borrowed list reads die.
                self.effect(&[list, index], &[value], None, Product::Owned, true);
            }
            I::ListRemoveAt { list, index } => {
                self.effect(&[list, index], &[], None, Product::Owned, true);
            }
            I::ListClear { list } => {
                self.effect(&[list], &[], None, Product::Owned, true);
            }
            I::DictionaryAdd {
                dictionary,
                key,
                value,
                ..
            }
            | I::DictionarySet {
                dictionary,
                key,
                value,
                ..
            } => {
                self.effect(&[dictionary], &[key, value], None, Product::Owned, false);
            }
            I::ListGet {
                destination,
                list,
                index,
                ..
            } => {
                self.effect(
                    &[list, index],
                    &[],
                    Some(destination),
                    Product::Borrowed,
                    false,
                );
            }
            I::DictionaryTryGet {
                destination,
                dictionary,
                key,
                ..
            } => {
                self.effect(
                    &[dictionary, key],
                    &[],
                    Some(destination),
                    Product::Borrowed,
                    false,
                );
            }
            _ => {}
        }
    }

    fn balance(&mut self, holders: &[usize]) {
        for (value, held) in holders.iter().enumerate() {
            if self.counted(value)
                && self.values[value].tokens != i64::try_from(*held).unwrap_or(i64::MAX)
            {
                self.flag("U1", "a value's counts differ from the slots that own it");
            }
        }
    }

    fn finish(&mut self, terminator: &mir::Terminator) {
        use mir::Terminator as T;
        match terminator {
            T::Goto(_) | T::Branch { .. } => {
                if let T::Branch { condition, .. } = terminator {
                    self.operand_values(condition);
                }
                let mut holders = vec![0; self.values.len()];
                for (index, (state, held)) in self.slots.iter().zip(&self.held).enumerate() {
                    // A slot whose bits are never read again holds no count.
                    let live = self.live_out.as_ref().is_none_or(|out| out[index]);
                    let uncounted =
                        self.frame.borrowed_params && self.frame.is_parameter_slot(index);
                    if let (SlotState::Live, Some(value)) = (state, held)
                        && live
                        && !uncounted
                    {
                        holders[*value] += 1;
                    }
                }
                self.balance(&holders);
            }
            T::Return(operand) => self.returning(operand.as_ref()),
            T::End => self.returning(None),
            T::Unreachable => {}
        }
    }

    fn returning(&mut self, operand: Option<&Operand>) {
        // Token balance only: every value ends with 0 tokens, the returned one
        // with 1. A live slot that still owned a count would leave tokens over.
        let mut returned = Vec::new();
        if let Some(Operand {
            type_,
            kind: OperandKind::Copy(place),
        }) = operand
        {
            self.touch_place(place);
            for (_, leaf) in self.frame.leaf_places(place, type_) {
                returned.push(self.read_leaf(&leaf));
            }
        }
        let mut holders = vec![0; self.values.len()];
        for value in returned {
            holders[value] += 1;
        }
        self.balance(&holders);
    }
}

fn successors(terminator: &mir::Terminator) -> Vec<BasicBlockId> {
    match terminator {
        mir::Terminator::Goto(target) => vec![*target],
        mir::Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
        _ => Vec::new(),
    }
}

#[allow(clippy::too_many_lines)]
fn check_function(
    module: &Module,
    native: &[SymbolId],
    function: &Function,
    findings: &mut Vec<OwnershipFinding>,
) {
    let mut frame = Frame {
        module,
        native,
        function,
        keys: Vec::new(),
        borrowed_params: false,
    };
    let mut keys = Vec::new();
    for local in function.parameters.iter().chain(&function.locals) {
        for (path, _) in frame.leaf_types(&local.type_) {
            keys.push((local.id, path));
        }
    }
    if keys.is_empty() {
        return;
    }
    if function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            matches!(
                instruction,
                mir::Instruction::OwnedRegionEnter { .. }
                    | mir::Instruction::OwnedRegionExit { .. }
                    | mir::Instruction::TemporarySubregionEnter { .. }
                    | mir::Instruction::TemporarySubregionExit { .. }
            )
        })
    {
        findings.push(OwnershipFinding {
            function: function.name.clone(),
            obligation: "U3",
            message: "a region rewind inside a function with counted references".to_owned(),
        });
    }
    frame.keys = keys;
    let mut found = analyse(&frame);
    if !found.is_empty() && leaf_function(function) {
        frame.borrowed_params = true;
        if analyse(&frame).is_empty() {
            found.clear();
        }
    }
    findings.extend(
        found
            .into_iter()
            .map(|(obligation, message)| OwnershipFinding {
                function: function.name.clone(),
                obligation,
                message,
            }),
    );
}

/// A function that can release nothing but its own parameters: no call of
/// any kind and no `Release` of anything but a parameter slot. Only there may
/// a parameter's original value be held without a count (C6, rule E3).
fn leaf_function(function: &Function) -> bool {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .all(|instruction| match instruction {
            mir::Instruction::Call { .. }
            | mir::Instruction::CallInterface { .. }
            | mir::Instruction::ForeignCall { .. }
            | mir::Instruction::CallIntrinsic { .. }
            | mir::Instruction::ListSet { .. }
            | mir::Instruction::ListRemoveAt { .. }
            | mir::Instruction::ListClear { .. } => false,
            mir::Instruction::Release { value } => {
                let OperandKind::Copy(place) = &value.kind else {
                    return true;
                };
                let mut root = place;
                while let Place::Field { base, .. } = root {
                    root = base;
                }
                matches!(root, Place::Local(id) if function.parameters.iter().any(|p| p.id == *id))
            }
            _ => true,
        })
}

type RawFinding = (&'static str, String);

/// Slot-state dataflow, slot liveness, and the per-block token check.
#[allow(clippy::too_many_lines)]
fn analyse(frame: &Frame<'_>) -> Vec<RawFinding> {
    let function = frame.function;
    let blocks = &function.blocks;
    let index: HashMap<BasicBlockId, usize> =
        blocks.iter().enumerate().map(|(i, b)| (b.id, i)).collect();
    let Some(&entry) = index.get(&function.entry) else {
        return Vec::new();
    };
    let initial: Vec<SlotState> = frame
        .keys
        .iter()
        .map(|(local, _)| {
            if function.parameters.iter().any(|p| p.id == *local) {
                SlotState::Live
            } else {
                SlotState::Null
            }
        })
        .collect();
    let first = !blocks
        .iter()
        .any(|b| successors(&b.terminator).contains(&function.entry));
    let mut states: Vec<Option<Vec<SlotState>>> = vec![None; blocks.len()];
    states[entry] = Some(initial);
    loop {
        let mut changed = false;
        for (i, block) in blocks.iter().enumerate() {
            let Some(state) = states[i].clone() else {
                continue;
            };
            let run = Block::run(frame, block, &state, i == entry && first, None);
            for target in successors(&block.terminator) {
                let Some(&j) = index.get(&target) else {
                    continue;
                };
                match &mut states[j] {
                    None => {
                        states[j] = Some(run.slots.clone());
                        changed = true;
                    }
                    Some(existing) => {
                        for (old, new) in existing.iter_mut().zip(&run.slots) {
                            let joined = old.join(*new);
                            if joined != *old {
                                *old = joined;
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    // Backward slot liveness from the per-block use/definition sets.
    let slots = frame.keys.len();
    let usage: Vec<Option<(Vec<bool>, Vec<bool>)>> = blocks
        .iter()
        .enumerate()
        .map(|(i, block)| {
            states[i].as_ref().map(|state| {
                let run = Block::run(frame, block, state, i == entry && first, None);
                (run.used, run.defined)
            })
        })
        .collect();
    let mut liveness: Vec<Liveness> = (0..blocks.len())
        .map(|_| Liveness {
            live_in: vec![false; slots],
            live_out: vec![false; slots],
        })
        .collect();
    loop {
        let mut changed = false;
        for (i, block) in blocks.iter().enumerate().rev() {
            let Some((used, defined)) = &usage[i] else {
                continue;
            };
            let mut out = vec![false; slots];
            for target in successors(&block.terminator) {
                if let Some(&j) = index.get(&target) {
                    for (slot, live) in out.iter_mut().zip(&liveness[j].live_in) {
                        *slot |= *live;
                    }
                }
            }
            let live_in: Vec<bool> = (0..slots)
                .map(|slot| used[slot] || (out[slot] && !defined[slot]))
                .collect();
            if live_in != liveness[i].live_in || out != liveness[i].live_out {
                liveness[i].live_in = live_in;
                liveness[i].live_out = out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut found = Vec::new();
    for (i, block) in blocks.iter().enumerate() {
        let Some(state) = &states[i] else {
            continue;
        };
        let run = Block::run(frame, block, state, i == entry && first, Some(&liveness[i]));
        found.extend(run.findings);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use mir::{AllocationRegion, Instruction, Rvalue, RvalueKind, Terminator, Visibility};

    fn class() -> Type {
        Type::Class(SymbolId(10))
    }

    fn local(id: u32, temporary: bool) -> mir::Local {
        mir::Local {
            id: LocalId(id),
            symbol: None,
            name: format!("l{id}"),
            type_: class(),
            mutable: true,
            temporary,
        }
    }

    fn lp(id: u32) -> Place {
        Place::Local(LocalId(id))
    }

    fn operand(place: Place) -> Operand {
        Operand {
            type_: class(),
            kind: OperandKind::Copy(place),
        }
    }

    fn field(id: u32) -> Place {
        Place::ObjectField {
            object: Box::new(operand(lp(id))),
            field: SymbolId(11),
        }
    }

    fn assign(target: Place, source: Place) -> Instruction {
        Instruction::Assign {
            target,
            value: Rvalue {
                type_: class(),
                kind: RvalueKind::Use(operand(source)),
            },
        }
    }

    fn retain(place: Place) -> Instruction {
        Instruction::Retain {
            value: operand(place),
        }
    }

    fn release(place: Place) -> Instruction {
        Instruction::Release {
            value: operand(place),
        }
    }

    fn allocate(id: u32, region: AllocationRegion) -> Instruction {
        Instruction::AllocateObject {
            destination: lp(id),
            class: SymbolId(10),
            region,
        }
    }

    fn obligations(
        parameters: u32,
        locals: &[(u32, bool)],
        instructions: Vec<Instruction>,
    ) -> Vec<&'static str> {
        obligations_ending(parameters, locals, instructions, Terminator::Return(None))
    }

    fn obligations_ending(
        parameters: u32,
        locals: &[(u32, bool)],
        instructions: Vec<Instruction>,
        terminator: Terminator,
    ) -> Vec<&'static str> {
        let module = Module {
            structs: Vec::new(),
            classes: vec![mir::ClassDefinition {
                symbol: SymbolId(10),
                name: "C".into(),
                fields: vec![mir::FieldDefinition {
                    symbol: SymbolId(11),
                    name: "f".into(),
                    type_: class(),
                }],
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
                parameters: (0..parameters).map(|id| local(id, false)).collect(),
                locals: locals
                    .iter()
                    .map(|(id, temporary)| local(*id, *temporary))
                    .collect(),
                return_type: Type::Void,
                entry: BasicBlockId(0),
                blocks: vec![BasicBlock {
                    id: BasicBlockId(0),
                    instructions,
                    terminator,
                }],
                temporary_subregion_candidates: Vec::new(),
            }],
        };
        match validate_ownership(&module) {
            Ok(()) => Vec::new(),
            Err(findings) => findings.iter().map(|finding| finding.obligation).collect(),
        }
    }

    #[test]
    fn balanced_sequence_has_no_findings() {
        let found = obligations(
            1,
            &[(1, false)],
            vec![
                retain(lp(0)),
                retain(lp(0)),
                assign(lp(1), lp(0)),
                release(lp(1)),
                release(lp(0)),
            ],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn release_of_captured_slot_without_a_count_is_u1() {
        let found = obligations(
            1,
            &[(1, true)],
            vec![
                retain(lp(0)),
                assign(lp(1), field(0)),
                release(lp(1)),
                retain(field(0)),
                release(lp(0)),
            ],
        );
        assert!(found.contains(&"U1"), "{found:?}");
    }

    #[test]
    fn use_of_heap_load_after_release_is_u2() {
        let found = obligations(
            1,
            &[(1, false)],
            vec![
                retain(lp(0)),
                assign(lp(1), field(0)),
                release(lp(0)),
                retain(lp(1)),
            ],
        );
        assert!(found.contains(&"U2"), "{found:?}");
    }

    #[test]
    fn store_of_borrowed_value_without_retain_is_u1() {
        let found = obligations(
            2,
            &[(2, true)],
            vec![
                retain(lp(0)),
                retain(lp(1)),
                assign(lp(2), field(0)),
                assign(field(0), field(1)),
                release(lp(2)),
                release(lp(0)),
                release(lp(1)),
            ],
        );
        assert!(
            !found.is_empty() && found.iter().all(|o| *o == "U1"),
            "{found:?}"
        );
    }

    #[test]
    fn release_of_unretained_parameter_is_u1() {
        assert!(obligations(1, &[], vec![release(lp(0))]).contains(&"U1"));
    }

    #[test]
    fn double_release_is_u1() {
        let found = obligations(1, &[], vec![retain(lp(0)), release(lp(0)), release(lp(0))]);
        assert!(found.contains(&"U1"), "{found:?}");
    }

    #[test]
    fn region_rewind_in_counted_code_is_u3() {
        let found = obligations(
            1,
            &[],
            vec![
                retain(lp(0)),
                mir::Instruction::TemporarySubregionExit {
                    id: mir::TemporarySubregionId(0),
                },
                release(lp(0)),
            ],
        );
        assert_eq!(found, vec!["U3"]);
    }

    #[test]
    fn reassignment_without_capture_leaks_u1() {
        let found = obligations(
            2,
            &[(2, false)],
            vec![
                retain(lp(0)),
                retain(lp(0)),
                assign(lp(2), lp(0)),
                retain(lp(1)),
                assign(lp(2), lp(1)),
                release(lp(0)),
                release(lp(1)),
                release(lp(2)),
            ],
        );
        assert!(found.contains(&"U1"), "{found:?}");
    }

    #[test]
    fn move_out_of_a_temporary_that_is_not_read_again_validates() {
        // t = this.f; x = t (the retain moved with the value); no release of t.
        let found = obligations(
            1,
            &[(1, true), (2, false)],
            vec![
                retain(lp(0)),
                retain(field(0)),
                assign(lp(1), field(0)),
                assign(lp(2), lp(1)),
                release(lp(0)),
                release(lp(2)),
            ],
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn move_out_of_a_temporary_that_is_read_again_is_rejected() {
        // The count moved to x, x is released, and t is read again: stale.
        let found = obligations(
            1,
            &[(1, true), (2, false), (3, false)],
            vec![
                retain(lp(0)),
                retain(field(0)),
                assign(lp(1), field(0)),
                assign(lp(2), lp(1)),
                release(lp(2)),
                retain(lp(1)),
                assign(lp(3), lp(1)),
                release(lp(3)),
                release(lp(0)),
            ],
        );
        assert!(found.contains(&"U1") || found.contains(&"U2"), "{found:?}");
    }

    #[test]
    fn parameter_elision_in_a_leaf_function_validates() {
        // return this.f with no entry retain and no exit release.
        let found = obligations_ending(
            1,
            &[(1, true)],
            vec![assign(lp(1), field(0)), retain(lp(1))],
            Terminator::Return(Some(operand(lp(1)))),
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn parameter_elision_in_a_function_with_a_call_is_u2() {
        // The call may release the object behind the parameter before it is read.
        let found = obligations(
            1,
            &[(1, false)],
            vec![
                Instruction::Call {
                    destination: None,
                    function: SymbolId(2),
                    arguments: Vec::new(),
                    return_type: Type::Void,
                },
                retain(lp(0)),
                assign(lp(1), lp(0)),
                release(lp(1)),
            ],
        );
        assert!(found.contains(&"U2"), "{found:?}");
    }

    #[test]
    fn leaked_slot_at_return_is_u1() {
        let found = obligations(1, &[(1, false)], vec![retain(lp(0)), assign(lp(1), lp(0))]);
        assert!(found.contains(&"U1"), "{found:?}");
    }

    #[test]
    fn temporary_allocation_stored_in_persistent_object_is_u4() {
        let found = obligations(
            0,
            &[(0, false), (1, false), (2, true)],
            vec![
                allocate(0, AllocationRegion::Persistent),
                allocate(1, AllocationRegion::Temporary),
                assign(lp(2), field(0)),
                assign(field(0), lp(1)),
                release(lp(2)),
                release(lp(0)),
                release(lp(1)),
            ],
        );
        assert_eq!(found, vec!["U4"]);
    }

    fn list_type() -> Type {
        Type::List(Box::new(class()))
    }

    fn list_operand() -> Operand {
        Operand {
            type_: list_type(),
            kind: OperandKind::Copy(lp(3)),
        }
    }

    fn zero() -> Operand {
        Operand {
            type_: Type::Int,
            kind: OperandKind::Constant(mir::Constant::Integer("0".into())),
        }
    }

    fn list_get(destination: u32) -> Instruction {
        Instruction::ListGet {
            destination: lp(destination),
            list: list_operand(),
            index: zero(),
            element_type: class(),
        }
    }

    fn use_call(id: u32) -> Instruction {
        Instruction::Call {
            destination: None,
            function: SymbolId(2),
            arguments: vec![operand(lp(id))],
            return_type: Type::Void,
        }
    }

    /// Parameter 0 is a class, local 1 receives list reads, parameter 2 is
    /// another class, and local 3 is the list.
    fn list_obligations(instructions: Vec<Instruction>) -> Vec<&'static str> {
        let mut list = local(3, false);
        list.type_ = list_type();
        let module = Module {
            structs: Vec::new(),
            classes: vec![mir::ClassDefinition {
                symbol: SymbolId(10),
                name: "C".into(),
                fields: vec![mir::FieldDefinition {
                    symbol: SymbolId(11),
                    name: "f".into(),
                    type_: class(),
                }],
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
                parameters: vec![local(0, false), local(2, false)],
                locals: vec![local(1, false), list],
                return_type: Type::Void,
                entry: BasicBlockId(0),
                blocks: vec![BasicBlock {
                    id: BasicBlockId(0),
                    instructions,
                    terminator: Terminator::Return(None),
                }],
                temporary_subregion_candidates: Vec::new(),
            }],
        };
        match validate_ownership(&module) {
            Ok(()) => Vec::new(),
            Err(findings) => findings.iter().map(|finding| finding.obligation).collect(),
        }
    }

    fn list_set() -> Instruction {
        Instruction::ListSet {
            list: list_operand(),
            index: zero(),
            value: operand(lp(2)),
        }
    }

    #[test]
    fn list_read_used_after_list_set_without_retain_is_rejected() {
        let found = list_obligations(vec![
            retain(lp(2)),
            list_get(1),
            retain(lp(2)),
            list_set(),
            use_call(1),
            release(lp(2)),
        ]);
        assert!(found.contains(&"U1") || found.contains(&"U2"), "{found:?}");
        assert!(found.contains(&"U2"), "{found:?}");
    }

    #[test]
    fn list_read_retained_survives_list_set() {
        let found = list_obligations(vec![
            retain(lp(2)),
            list_get(1),
            retain(lp(1)),
            retain(lp(2)),
            list_set(),
            use_call(1),
            release(lp(1)),
            release(lp(2)),
        ]);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn list_remove_at_and_clear_invalidate_list_reads() {
        for invalidating in [
            Instruction::ListRemoveAt {
                list: list_operand(),
                index: zero(),
            },
            Instruction::ListClear {
                list: list_operand(),
            },
        ] {
            let found = list_obligations(vec![list_get(1), invalidating, use_call(1)]);
            assert!(found.contains(&"U2"), "{found:?}");
        }
    }
}
