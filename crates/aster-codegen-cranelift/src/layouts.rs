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

pub(super) struct Layouts {
    pub(super) pointer_bytes: u32,
    pub(super) structs: HashMap<mir::SymbolId, mir::StructDefinition>,
    pub(super) enums: HashMap<mir::SymbolId, mir::EnumDefinition>,
    pub(super) types: HashMap<mir::SymbolId, TypeLayout>,
    pub(super) fields: HashMap<mir::SymbolId, FieldLayout>,
}

impl Layouts {
    pub(super) fn new(module: &mir::Module, pointer_bytes: u32) -> Result<Self, BackendError> {
        let mut layouts = Self {
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
        let mut offset = 0_u32;
        let mut alignment = 1_u32;
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
            functions: Vec::new(),
        };
        let layouts = Layouts::new(&module, 8).expect("class layout");
        assert_eq!(layouts.fields[&value].offset, 0);
        assert_eq!(layouts.fields[&next].offset, 8);
        assert_eq!(layouts.types[&class].size, 16);
    }

    fn empty_module() -> mir::Module {
        mir::Module {
            structs: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            enums: Vec::new(),
            interface_implementations: Vec::new(),
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
