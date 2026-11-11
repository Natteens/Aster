use super::{BackendError, HashMap, Primitive, mir, primitive};

#[derive(Clone, Debug)]
pub(super) struct FieldLayout {
    pub(super) offset: u32,
    pub(super) type_: mir::Type,
}

#[derive(Clone, Debug)]
pub(super) struct TypeLayout {
    pub(super) size: u32,
    pub(super) align_shift: u8,
}

#[derive(Clone, Debug)]
pub(super) struct TaskArgumentLayout {
    pub(super) offsets: Vec<u32>,
    pub(super) size: u32,
}

#[cfg(any(
    all(feature = "mem-exp-h8", feature = "mem-exp-h12"),
    all(feature = "mem-exp-h8", feature = "mem-exp-h16"),
    all(feature = "mem-exp-h12", feature = "mem-exp-h16"),
))]
compile_error!("enable at most one MEM-EXP-1 header feature");

/// MEM-EXP-1 header configuration: bytes before a class object's first field.
/// Zero (C0) is the production layout.
pub(super) const OBJECT_HEADER_BYTES: u32 = if cfg!(feature = "mem-exp-h16") {
    16
} else if cfg!(feature = "mem-exp-h12") {
    12
} else if cfg!(feature = "mem-exp-h8") {
    8
} else {
    0
};

/// Per-class `ObjectDescriptor`s owned by the prepared program. Boxed so the
/// addresses written into objects stay stable when the store is moved, and
/// kept alive until the JIT module and every execution context are gone.
#[cfg(feature = "mem-exp-rc-core")]
#[derive(Default)]
pub(super) struct ObjectDescriptors {
    #[allow(dead_code, reason = "owns the offset tables the descriptors point at")]
    offsets: Vec<Box<[u32]>>,
    #[allow(
        dead_code,
        clippy::vec_box,
        reason = "boxing keeps descriptor addresses stable"
    )]
    descriptors: Vec<Box<aster_runtime::object_heap::ObjectDescriptor>>,
    by_class: HashMap<mir::SymbolId, *const aster_runtime::object_heap::ObjectDescriptor>,
    /// Element descriptors of `List<T>` and `T[]` types with reference leaves,
    /// keyed by `mir::type_key(T)` (design sections 7 and 8).
    by_element: HashMap<u64, *const aster_runtime::object_heap::ObjectDescriptor>,
    #[allow(
        dead_code,
        clippy::vec_box,
        reason = "boxing keeps descriptor-pair addresses stable"
    )]
    pairs: Vec<Box<aster_runtime::object_heap::DictionaryDescriptors>>,
    /// `{ key, value }` descriptor pairs of `Dictionary<K, V>` types with
    /// reference leaves, keyed by the two `mir::type_key`s (design section 11).
    by_dictionary: HashMap<(u64, u64), *const aster_runtime::object_heap::DictionaryDescriptors>,
}

#[cfg(feature = "mem-exp-rc-core")]
impl ObjectDescriptors {
    #[allow(clippy::too_many_lines)]
    pub(super) fn build(layouts: &Layouts, module: &mir::Module) -> Result<Self, BackendError> {
        let mut store = Self::default();
        // `StringBuilder` objects have a native header without a count word,
        // so a field holding one is never released through a descriptor.
        let native = module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                mir::Instruction::AllocateStringBuilder { class, .. }
                | mir::Instruction::StringBuilderAppend { class, .. }
                | mir::Instruction::StringBuilderToString { class, .. } => Some(*class),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        for class in &module.classes {
            let size = layouts
                .types
                .get(&class.symbol)
                .ok_or_else(|| BackendError::new("class has no computed object layout"))?
                .size;
            let offsets = layouts
                .reference_offsets(class.symbol, &native)?
                .into_boxed_slice();
            let descriptor = Box::new(aster_runtime::object_heap::ObjectDescriptor {
                size,
                reference_count: u32::try_from(offsets.len())
                    .map_err(|_| BackendError::new("class has too many reference fields"))?,
                reference_offsets: offsets.as_ptr(),
            });
            store
                .by_class
                .insert(class.symbol, std::ptr::from_ref(&*descriptor));
            store.offsets.push(offsets);
            store.descriptors.push(descriptor);
        }
        let mut element_types = Vec::new();
        let mut dictionaries = Vec::new();
        for instruction in module
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
        {
            match instruction {
                mir::Instruction::AllocateList { element_type, .. }
                | mir::Instruction::AllocateArray { element_type, .. } => {
                    element_types.push(element_type);
                }
                mir::Instruction::DictionaryKeys { key_type, .. } => element_types.push(key_type),
                mir::Instruction::DictionaryValues { value_type, .. } => {
                    element_types.push(value_type);
                }
                mir::Instruction::DictionaryEntries { entry_type, .. } => {
                    element_types.push(entry_type);
                }
                mir::Instruction::AllocateDictionary {
                    key_type,
                    value_type,
                    ..
                } => {
                    element_types.push(key_type);
                    element_types.push(value_type);
                    dictionaries.push((key_type, value_type));
                }
                _ => {}
            }
        }
        for element_type in element_types {
            let key = mir::type_key(element_type);
            if store.by_element.contains_key(&key) {
                continue;
            }
            let mut offsets = Vec::new();
            layouts.collect_type_offsets(element_type, 0, &native, &mut offsets)?;
            if offsets.is_empty() {
                continue;
            }
            let offsets = offsets.into_boxed_slice();
            let descriptor = Box::new(aster_runtime::object_heap::ObjectDescriptor {
                size: layouts.type_layout(element_type)?.size,
                reference_count: u32::try_from(offsets.len())
                    .map_err(|_| BackendError::new("element has too many reference leaves"))?,
                reference_offsets: offsets.as_ptr(),
            });
            store
                .by_element
                .insert(key, std::ptr::from_ref(&*descriptor));
            store.offsets.push(offsets);
            store.descriptors.push(descriptor);
        }
        for (key_type, value_type) in dictionaries {
            let key = (mir::type_key(key_type), mir::type_key(value_type));
            if store.by_dictionary.contains_key(&key) {
                continue;
            }
            let key_descriptor = store.by_element.get(&key.0).copied();
            let value_descriptor = store.by_element.get(&key.1).copied();
            if key_descriptor.is_none() && value_descriptor.is_none() {
                continue;
            }
            let pair = Box::new(aster_runtime::object_heap::DictionaryDescriptors {
                key: key_descriptor.unwrap_or(std::ptr::null()),
                value: value_descriptor.unwrap_or(std::ptr::null()),
            });
            store.by_dictionary.insert(key, std::ptr::from_ref(&*pair));
            store.pairs.push(pair);
        }
        Ok(store)
    }

    /// Address of the `{ key, value }` descriptor pair of
    /// `Dictionary<key_type, value_type>`, or `None` when neither type has
    /// reference leaves.
    pub(super) fn dictionary_address(
        &self,
        key_type: &mir::Type,
        value_type: &mir::Type,
    ) -> Option<u64> {
        self.by_dictionary
            .get(&(mir::type_key(key_type), mir::type_key(value_type)))
            .map(|pointer| *pointer as u64)
    }

    /// Element descriptor address for `List<element_type>` or
    /// `element_type[]`, or `None` when the element type has no reference
    /// leaves.
    pub(super) fn element_address(&self, element_type: &mir::Type) -> Option<u64> {
        self.by_element
            .get(&mir::type_key(element_type))
            .map(|pointer| *pointer as u64)
    }

    pub(super) fn address(&self, class: mir::SymbolId) -> Option<u64> {
        self.by_class.get(&class).map(|pointer| *pointer as u64)
    }
}

pub(super) struct Layouts {
    classes: std::collections::HashSet<mir::SymbolId>,
    pub(super) pointer_bytes: u32,
    pub(super) structs: HashMap<mir::SymbolId, mir::StructDefinition>,
    pub(super) enums: HashMap<mir::SymbolId, mir::EnumDefinition>,
    pub(super) types: HashMap<mir::SymbolId, TypeLayout>,
    pub(super) fields: HashMap<mir::SymbolId, FieldLayout>,
}

impl Layouts {
    pub(super) fn new(module: &mir::Module, pointer_bytes: u32) -> Result<Self, BackendError> {
        let mut layouts = Self {
            classes: module.classes.iter().map(|class| class.symbol).collect(),
            pointer_bytes,
            structs: module
                .structs
                .iter()
                .map(|definition| (definition.symbol, definition.clone()))
                .chain(module.classes.iter().map(|definition| {
                    (
                        definition.symbol,
                        mir::StructDefinition {
                            symbol: definition.symbol,
                            name: definition.name.clone(),
                            fields: definition.fields.clone(),
                        },
                    )
                }))
                .collect(),
            enums: module
                .enums
                .iter()
                .map(|definition| (definition.symbol, definition.clone()))
                .collect(),
            types: HashMap::new(),
            fields: HashMap::new(),
        };
        let symbols = layouts.structs.keys().copied().collect::<Vec<_>>();
        for symbol in symbols {
            layouts.compute_struct(symbol, &mut Vec::new())?;
        }
        let enum_symbols = layouts.enums.keys().copied().collect::<Vec<_>>();
        for symbol in enum_symbols {
            layouts.compute_enum(symbol, &mut Vec::new())?;
        }
        Ok(layouts)
    }

    /// Whether a value of `type_` holds at least one counted reference leaf.
    #[cfg(feature = "mem-exp-rc-core")]
    pub(super) fn type_has_references(&self, type_: &mir::Type) -> bool {
        match type_ {
            mir::Type::Class(_)
            | mir::Type::Interface(_)
            | mir::Type::Array(_)
            | mir::Type::List(_)
            | mir::Type::Dictionary(_, _)
            | mir::Type::String => true,
            mir::Type::User(symbol) => self.structs.get(symbol).is_some_and(|definition| {
                definition
                    .fields
                    .iter()
                    .any(|field| self.type_has_references(&field.type_))
            }),
            _ => false,
        }
    }

    /// Byte offsets, from the object start, of every reference leaf of a
    /// class, including leaves of inline struct fields.
    #[cfg(feature = "mem-exp-rc-core")]
    pub(super) fn reference_offsets(
        &self,
        class: mir::SymbolId,
        native: &std::collections::HashSet<mir::SymbolId>,
    ) -> Result<Vec<u32>, BackendError> {
        let mut offsets = Vec::new();
        self.collect_reference_offsets(class, 0, native, &mut offsets)?;
        Ok(offsets)
    }

    #[cfg(feature = "mem-exp-rc-core")]
    fn collect_reference_offsets(
        &self,
        aggregate: mir::SymbolId,
        base: u32,
        native: &std::collections::HashSet<mir::SymbolId>,
        offsets: &mut Vec<u32>,
    ) -> Result<(), BackendError> {
        let definition = self
            .structs
            .get(&aggregate)
            .ok_or_else(|| BackendError::new("unknown executable struct type"))?;
        for field in &definition.fields {
            let offset = base
                + self
                    .fields
                    .get(&field.symbol)
                    .ok_or_else(|| BackendError::new("field has no computed layout"))?
                    .offset;
            self.collect_type_offsets(&field.type_, offset, native, offsets)?;
        }
        Ok(())
    }

    /// Leaf offsets of one value of `type_` placed at `offset`: a class,
    /// interface, array, list, dictionary or string is one leaf (its object word), an inline struct contributes
    /// its fields' leaves, everything else has none.
    #[cfg(feature = "mem-exp-rc-core")]
    pub(super) fn collect_type_offsets(
        &self,
        type_: &mir::Type,
        offset: u32,
        native: &std::collections::HashSet<mir::SymbolId>,
        offsets: &mut Vec<u32>,
    ) -> Result<(), BackendError> {
        match type_ {
            mir::Type::Class(symbol) if native.contains(symbol) => {}
            mir::Type::Class(_)
            | mir::Type::Interface(_)
            | mir::Type::Array(_)
            | mir::Type::List(_)
            | mir::Type::Dictionary(_, _)
            | mir::Type::String => {
                offsets.push(offset);
            }
            mir::Type::User(inner) if self.structs.contains_key(inner) => {
                self.collect_reference_offsets(*inner, offset, native, offsets)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn compute_enum(
        &mut self,
        symbol: mir::SymbolId,
        visiting: &mut Vec<mir::SymbolId>,
    ) -> Result<TypeLayout, BackendError> {
        if let Some(layout) = self.types.get(&symbol) {
            return Ok(layout.clone());
        }
        if visiting.contains(&symbol) {
            return Err(BackendError::new("recursive enum layout reached the JIT"));
        }
        let definition = self
            .enums
            .get(&symbol)
            .cloned()
            .ok_or_else(|| BackendError::new("unknown executable enum type"))?;
        visiting.push(symbol);
        let mut payload_size = 0_u32;
        let mut payload_alignment = 1_u32;
        let mut case_layouts = Vec::new();
        for case in definition.cases {
            let mut offset = 0_u32;
            let mut fields = Vec::new();
            for field in case.fields {
                let layout = self.layout_of(&field.type_, visiting)?;
                let alignment = 1_u32 << layout.align_shift;
                payload_alignment = payload_alignment.max(alignment);
                offset = align_up(offset, alignment)?;
                fields.push((field, offset));
                offset = offset
                    .checked_add(layout.size)
                    .ok_or_else(|| BackendError::new("enum payload layout is too large"))?;
            }
            payload_size = payload_size.max(offset);
            case_layouts.push(fields);
        }
        let payload_offset = align_up(4, payload_alignment)?;
        for fields in case_layouts {
            for (field, offset) in fields {
                self.fields.insert(
                    field.symbol,
                    FieldLayout {
                        offset: payload_offset + offset,
                        type_: field.type_,
                    },
                );
            }
        }
        visiting.pop();
        let alignment = 4_u32.max(payload_alignment);
        let size = align_up(payload_offset + payload_size, alignment)?;
        let layout = TypeLayout {
            size,
            align_shift: u8::try_from(alignment.trailing_zeros())
                .map_err(|_| BackendError::new("enum alignment is too large"))?,
        };
        self.types.insert(symbol, layout.clone());
        Ok(layout)
    }

    fn compute_struct(
        &mut self,
        symbol: mir::SymbolId,
        visiting: &mut Vec<mir::SymbolId>,
    ) -> Result<TypeLayout, BackendError> {
        if let Some(layout) = self.types.get(&symbol) {
            return Ok(layout.clone());
        }
        if visiting.contains(&symbol) {
            return Err(BackendError::new("recursive struct layout reached the JIT"));
        }
        let definition = self.structs.get(&symbol).cloned().ok_or_else(|| {
            BackendError::new(format!("user type {symbol:?} is not an executable struct"))
        })?;
        visiting.push(symbol);
        let header = if self.classes.contains(&symbol) {
            OBJECT_HEADER_BYTES
        } else {
            0
        };
        let mut offset = header;
        let mut alignment = if header == 0 { 1_u32 } else { 8 };
        for field in definition.fields {
            let field_layout = self.layout_of(&field.type_, visiting)?;
            let field_alignment = 1_u32 << field_layout.align_shift;
            alignment = alignment.max(field_alignment);
            offset = align_up(offset, field_alignment)?;
            self.fields.insert(
                field.symbol,
                FieldLayout {
                    offset,
                    type_: field.type_,
                },
            );
            offset = offset
                .checked_add(field_layout.size)
                .ok_or_else(|| BackendError::new("struct layout exceeds addressable size"))?;
        }
        visiting.pop();
        let size = if offset == 0 {
            1
        } else {
            align_up(offset, alignment)?
        };
        let layout = TypeLayout {
            size,
            align_shift: u8::try_from(alignment.trailing_zeros())
                .map_err(|_| BackendError::new("struct alignment is too large"))?,
        };
        self.types.insert(symbol, layout.clone());
        Ok(layout)
    }

    fn layout_of(
        &mut self,
        type_: &mir::Type,
        visiting: &mut Vec<mir::SymbolId>,
    ) -> Result<TypeLayout, BackendError> {
        if let mir::Type::User(symbol) = type_ {
            return self.compute_struct(*symbol, visiting);
        }
        if let mir::Type::Enum(symbol) = type_ {
            return self.compute_enum(*symbol, visiting);
        }
        if let mir::Type::Interface(_) = type_ {
            return Ok(TypeLayout {
                size: self.pointer_bytes * 2,
                align_shift: u8::try_from(self.pointer_bytes.trailing_zeros())
                    .map_err(|_| BackendError::new("pointer alignment is too large"))?,
            });
        }
        if matches!(
            type_,
            mir::Type::Array(_)
                | mir::Type::Class(_)
                | mir::Type::List(_)
                | mir::Type::Dictionary(_, _)
        ) {
            return Ok(TypeLayout {
                size: self.pointer_bytes,
                align_shift: u8::try_from(self.pointer_bytes.trailing_zeros())
                    .map_err(|_| BackendError::new("pointer alignment is too large"))?,
            });
        }
        // `Task<T>` is a plain `i64` handle id (see `declarations::clif_value_type`),
        // never a pointer.
        if matches!(type_, mir::Type::Task(_)) {
            return Ok(TypeLayout {
                size: 8,
                align_shift: 3,
            });
        }
        let size = match primitive(type_) {
            Some(Primitive::String) => self.pointer_bytes,
            Some(Primitive::Decimal) => {
                return Err(BackendError::new(
                    "`decimal` cannot be used in an executable struct until its runtime layout exists",
                ));
            }
            Some(primitive) => {
                u32::from(primitive.bit_width().expect("fixed scalar has a width") / 8)
            }
            None => return Err(BackendError::new("non-value type has no runtime layout")),
        };
        Ok(TypeLayout {
            size,
            align_shift: u8::try_from(size.trailing_zeros())
                .map_err(|_| BackendError::new("scalar alignment is too large"))?,
        })
    }

    pub(super) fn type_layout(&self, type_: &mir::Type) -> Result<TypeLayout, BackendError> {
        if let mir::Type::User(symbol) = type_ {
            return self
                .types
                .get(symbol)
                .cloned()
                .ok_or_else(|| BackendError::new("unknown executable struct type"));
        }
        if let mir::Type::Enum(symbol) = type_ {
            return self
                .types
                .get(symbol)
                .cloned()
                .ok_or_else(|| BackendError::new("unknown executable enum type"));
        }
        if let mir::Type::Interface(_) = type_ {
            return Ok(TypeLayout {
                size: self.pointer_bytes * 2,
                align_shift: u8::try_from(self.pointer_bytes.trailing_zeros())
                    .map_err(|_| BackendError::new("pointer alignment is too large"))?,
            });
        }
        if matches!(
            type_,
            mir::Type::Array(_)
                | mir::Type::Class(_)
                | mir::Type::List(_)
                | mir::Type::Dictionary(_, _)
        ) {
            return Ok(TypeLayout {
                size: self.pointer_bytes,
                align_shift: u8::try_from(self.pointer_bytes.trailing_zeros())
                    .map_err(|_| BackendError::new("pointer alignment is too large"))?,
            });
        }
        // `Task<T>` is a plain `i64` handle id (see `declarations::clif_value_type`),
        // never a pointer.
        if matches!(type_, mir::Type::Task(_)) {
            return Ok(TypeLayout {
                size: 8,
                align_shift: 3,
            });
        }
        let size = match primitive(type_) {
            Some(Primitive::String) => self.pointer_bytes,
            Some(primitive) => u32::from(
                primitive
                    .bit_width()
                    .ok_or_else(|| BackendError::new("type has no executable layout"))?
                    / 8,
            ),
            None => return Err(BackendError::new("type has no executable layout")),
        };
        Ok(TypeLayout {
            size,
            align_shift: u8::try_from(size.trailing_zeros())
                .map_err(|_| BackendError::new("scalar alignment is too large"))?,
        })
    }

    /// Stable, eight-byte-aligned host transfer frame used by Task.Run.
    /// Each concrete argument keeps its ordinary ASTER size/alignment; the
    /// frame itself is rounded to a word so the runtime can own it as
    /// `Vec<u64>` without retaining caller storage.
    pub(super) fn task_argument_layout(
        &self,
        parameters: &[mir::Type],
    ) -> Result<TaskArgumentLayout, BackendError> {
        let mut offsets = Vec::with_capacity(parameters.len());
        let mut size = 0_u32;
        for parameter in parameters {
            let layout = self.type_layout(parameter)?;
            let alignment = 1_u32 << layout.align_shift;
            size = align_up(size, alignment)?;
            offsets.push(size);
            size = size
                .checked_add(layout.size)
                .ok_or_else(|| BackendError::new("Task.Run argument frame is too large"))?;
        }
        let size = if size == 0 { 0 } else { align_up(size, 8)? };
        Ok(TaskArgumentLayout { offsets, size })
    }

    pub(super) fn zero_initializable(&self, type_: &mir::Type) -> bool {
        match type_ {
            mir::Type::String
            | mir::Type::Decimal
            | mir::Type::Array(_)
            | mir::Type::Class(_)
            | mir::Type::Interface(_)
            | mir::Type::Enum(_)
            | mir::Type::List(_)
            | mir::Type::Dictionary(_, _)
            | mir::Type::Void
            | mir::Type::Unknown => false,
            mir::Type::User(symbol) => self.structs.get(symbol).is_some_and(|definition| {
                definition
                    .fields
                    .iter()
                    .all(|field| self.zero_initializable(&field.type_))
            }),
            _ => true,
        }
    }
}

fn align_up(value: u32, alignment: u32) -> Result<u32, BackendError> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| BackendError::new("struct layout exceeds addressable size"))
}

#[cfg(test)]
mod layout_tests {
    use super::Layouts;
    use aster_mir as mir;

    #[test]
    fn lays_out_struct_fields_with_natural_alignment() {
        let struct_symbol = mir::SymbolId(1);
        let byte = mir::SymbolId(2);
        let long = mir::SymbolId(3);
        let short = mir::SymbolId(4);
        let module = mir::Module {
            enums: Vec::new(),
            structs: vec![mir::StructDefinition {
                symbol: struct_symbol,
                name: "Mixed".to_owned(),
                fields: vec![
                    mir::FieldDefinition {
                        symbol: byte,
                        name: "a".to_owned(),
                        type_: mir::Type::Byte,
                    },
                    mir::FieldDefinition {
                        symbol: long,
                        name: "b".to_owned(),
                        type_: mir::Type::Long,
                    },
                    mir::FieldDefinition {
                        symbol: short,
                        name: "c".to_owned(),
                        type_: mir::Type::Short,
                    },
                ],
            }],
            classes: Vec::new(),
            interfaces: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: Vec::new(),
        };
        let layouts = Layouts::new(&module, 8).expect("finite layout");
        assert_eq!(layouts.fields[&byte].offset, 0);
        assert_eq!(layouts.fields[&long].offset, 8);
        assert_eq!(layouts.fields[&short].offset, 16);
        assert_eq!(layouts.types[&struct_symbol].size, 24);
        assert_eq!(layouts.types[&struct_symbol].align_shift, 3);
    }

    #[test]
    fn lays_out_class_references_as_pointers() {
        let class = mir::SymbolId(10);
        let value = mir::SymbolId(11);
        let next = mir::SymbolId(12);
        let module = mir::Module {
            enums: Vec::new(),
            structs: Vec::new(),
            classes: vec![mir::ClassDefinition {
                symbol: class,
                name: "Node".to_owned(),
                fields: vec![
                    mir::FieldDefinition {
                        symbol: value,
                        name: "value".to_owned(),
                        type_: mir::Type::Int,
                    },
                    mir::FieldDefinition {
                        symbol: next,
                        name: "next".to_owned(),
                        type_: mir::Type::Class(class),
                    },
                ],
            }],
            interfaces: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: Vec::new(),
        };
        let layouts = Layouts::new(&module, 8).expect("class layout");
        // Exact per MEM-EXP-1 configuration: C0 (no header) 0/8/16, H8 8/16/24,
        // H12 12/16/24, H16 16/24/32.
        let header = super::OBJECT_HEADER_BYTES;
        let next_offset = (header + 4).next_multiple_of(8);
        assert_eq!(layouts.fields[&value].offset, header);
        assert_eq!(layouts.fields[&next].offset, next_offset);
        assert_eq!(layouts.types[&class].size, next_offset + 8);
    }

    fn empty_module() -> mir::Module {
        mir::Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
            foreign_functions: Vec::new(),
            functions: Vec::new(),
        }
    }

    #[test]
    fn list_of_int_is_pointer_sized_on_a_64_bit_target() {
        let layouts = Layouts::new(&empty_module(), 8).expect("empty module always lays out");
        let layout = layouts
            .type_layout(&mir::Type::List(Box::new(mir::Type::Int)))
            .expect("List<T> has a pointer layout regardless of T");
        assert_eq!(layout.size, 8);
        assert_eq!(layout.align_shift, 3);
    }

    #[test]
    fn list_of_int_is_pointer_sized_on_a_32_bit_target() {
        let layouts = Layouts::new(&empty_module(), 4).expect("empty module always lays out");
        let layout = layouts
            .type_layout(&mir::Type::List(Box::new(mir::Type::Int)))
            .expect("List<T> has a pointer layout regardless of T");
        assert_eq!(layout.size, 4);
        assert_eq!(layout.align_shift, 2);
    }

    #[test]
    fn list_layout_does_not_depend_on_its_element_type() {
        let layouts = Layouts::new(&empty_module(), 8).expect("empty module always lays out");
        let of_int = layouts
            .type_layout(&mir::Type::List(Box::new(mir::Type::Int)))
            .expect("List<int>");
        let of_string = layouts
            .type_layout(&mir::Type::List(Box::new(mir::Type::String)))
            .expect("List<string>");
        let nested = layouts
            .type_layout(&mir::Type::List(Box::new(mir::Type::List(Box::new(
                mir::Type::Long,
            )))))
            .expect("List<List<long>>");
        assert_eq!(of_int.size, of_string.size);
        assert_eq!(of_int.size, nested.size);
        assert_eq!(of_int.align_shift, nested.align_shift);
    }

    #[test]
    fn list_is_never_zero_initializable() {
        let layouts = Layouts::new(&empty_module(), 8).expect("empty module always lays out");
        assert!(!layouts.zero_initializable(&mir::Type::List(Box::new(mir::Type::Int))));
    }

    #[test]
    fn task_argument_frames_align_mixed_widths_and_round_to_owned_words() {
        let layouts = Layouts::new(&empty_module(), 8).expect("empty module always lays out");
        let layout = layouts
            .task_argument_layout(&[
                mir::Type::Byte,
                mir::Type::Long,
                mir::Type::Bool,
                mir::Type::Double,
            ])
            .expect("mixed scalar arguments have a finite frame");

        assert_eq!(layout.offsets, vec![0, 8, 16, 24]);
        assert_eq!(layout.size, 32);
        assert!(super::align_up(u32::MAX, 8).is_err());
    }
}
