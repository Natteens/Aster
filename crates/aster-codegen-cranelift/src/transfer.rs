//! Deep-copy transfer support in the Cranelift backend (CONC-005).
//!
//! Builds the runtime-facing [`TransferShape`] of a MIR type from the
//! backend's own layouts, interns its encoding as read-only JIT data and
//! provides the small emission helpers used by the `Task.Run`, `Wait` and
//! `WaitAll` lowering. The runtime (`aster-runtime`) does the actual
//! serialization and materialization; nothing here is domain-specific.

use aster_runtime::{TransferCase, TransferShape};

use super::{
    BackendError, Codegen, DataDescription, FunctionBuilder, InstBuilder, MemFlags, Module,
    StackSlotData, StackSlotKind, Value, is_aggregate, mir, module_error, primitive, types,
};

/// Result types a task can return by deep copy (everything that is not a
/// plain scalar): strings, collections and structs/enums.
pub(super) fn is_deep_result_type(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::String
            | mir::Type::Array(_)
            | mir::Type::List(_)
            | mir::Type::Dictionary(_, _)
            | mir::Type::User(_)
            | mir::Type::Enum(_)
    )
}

impl Codegen {
    /// The transfer shape of `type_`. Classes, interfaces, tasks, `decimal`
    /// and `void` are not transferable and yield an error (validation rejects
    /// them earlier; this stays defensive).
    pub(super) fn transfer_shape(&self, type_: &mir::Type) -> Result<TransferShape, BackendError> {
        Ok(match type_ {
            mir::Type::String => TransferShape::String,
            mir::Type::Array(element) => TransferShape::Array {
                element: Box::new(self.transfer_shape(element)?),
                descriptor: self.element_descriptor_bits(element),
            },
            mir::Type::List(element) => TransferShape::List {
                element: Box::new(self.transfer_shape(element)?),
                descriptor: self.element_descriptor_bits(element),
            },
            mir::Type::Dictionary(key, value) => TransferShape::Dictionary {
                key: Box::new(self.transfer_shape(key)?),
                value: Box::new(self.transfer_shape(value)?),
                descriptors: self.dictionary_descriptor_bits(key, value),
            },
            mir::Type::User(symbol) => {
                let definition = self
                    .layouts
                    .structs
                    .get(symbol)
                    .ok_or_else(|| BackendError::new("unknown struct in a transfer shape"))?;
                let mut fields = Vec::new();
                for field in &definition.fields {
                    fields.push((
                        self.field_offset(field.symbol)?,
                        self.transfer_shape(&field.type_)?,
                    ));
                }
                TransferShape::Struct {
                    size: self.layouts.type_layout(type_)?.size,
                    fields,
                }
            }
            mir::Type::Enum(symbol) => {
                let definition = self
                    .layouts
                    .enums
                    .get(symbol)
                    .ok_or_else(|| BackendError::new("unknown enum in a transfer shape"))?;
                let mut cases = Vec::new();
                for case in &definition.cases {
                    let mut fields = Vec::new();
                    for field in &case.fields {
                        fields.push((
                            self.field_offset(field.symbol)?,
                            self.transfer_shape(&field.type_)?,
                        ));
                    }
                    cases.push(TransferCase {
                        tag: case.tag,
                        fields,
                    });
                }
                TransferShape::Enum {
                    size: self.layouts.type_layout(type_)?.size,
                    cases,
                }
            }
            other => match primitive(other) {
                Some(primitive) if primitive.is_worker_transferable() => TransferShape::Scalar {
                    size: self.layouts.type_layout(other)?.size,
                },
                _ => {
                    return Err(BackendError::new(format!(
                        "type `{}` cannot cross a worker boundary",
                        super::type_name(other)
                    )));
                }
            },
        })
    }

    fn field_offset(&self, symbol: mir::SymbolId) -> Result<u32, BackendError> {
        self.layouts
            .fields
            .get(&symbol)
            .map(|layout| layout.offset)
            .ok_or_else(|| BackendError::new("field has no computed layout"))
    }

    /// Element descriptor address of the counting configuration (`0` when the
    /// element has no reference leaves or counting is not compiled in).
    #[allow(unused_variables, clippy::unused_self)]
    pub(super) fn element_descriptor_bits(&self, element: &mir::Type) -> u64 {
        #[cfg(feature = "mem-exp-rc-core")]
        {
            self.descriptors.element_address(element).unwrap_or(0)
        }
        #[cfg(not(feature = "mem-exp-rc-core"))]
        {
            0
        }
    }

    #[allow(unused_variables, clippy::unused_self)]
    fn dictionary_descriptor_bits(&self, key: &mir::Type, value: &mir::Type) -> u64 {
        #[cfg(feature = "mem-exp-rc-core")]
        {
            self.descriptors.dictionary_address(key, value).unwrap_or(0)
        }
        #[cfg(not(feature = "mem-exp-rc-core"))]
        {
            0
        }
    }

    /// Whether a value of `type_` holds runtime-owned references (so it must
    /// cross by deep copy instead of a raw frame copy).
    pub(super) fn type_needs_transfer(&self, type_: &mir::Type) -> Result<bool, BackendError> {
        if !is_deep_result_type(type_) {
            return Ok(false);
        }
        Ok(self.transfer_shape(type_)?.has_references())
    }

    /// Pointer and byte length of the interned encoding of `shape`.
    pub(super) fn transfer_shape_operands(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        shape: &TransferShape,
    ) -> Result<(Value, Value), BackendError> {
        let bytes = shape.encode();
        let length = i64::try_from(bytes.len())
            .map_err(|_| BackendError::new("transfer shape is too large"))?;
        let id = if let Some(id) = self.transfer_data.get(&bytes) {
            *id
        } else {
            let id = self
                .jit
                .declare_anonymous_data(false, false)
                .map_err(module_error)?;
            let mut description = DataDescription::new();
            description.define(bytes.clone().into_boxed_slice());
            description.set_align(8);
            self.jit
                .define_data(id, &description)
                .map_err(module_error)?;
            self.transfer_data.insert(bytes, id);
            id
        };
        let global = self.jit.declare_data_in_func(id, builder.func);
        let pointer = builder.ins().global_value(self.pointer_type, global);
        let length = builder.ins().iconst(types::I32, length);
        Ok((pointer, length))
    }

    /// Address of the memory image of `value`: the value itself for inline
    /// aggregates, otherwise a fresh stack slot holding the pointer.
    pub(super) fn value_image_address(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        type_: &mir::Type,
        value: Value,
    ) -> Value {
        if is_aggregate(type_) {
            return value;
        }
        let slot =
            builder.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
        builder.ins().stack_store(value, slot, 0);
        builder.ins().stack_addr(self.pointer_type, slot, 0)
    }

    /// A zero-initialized writable image slot for a value of `type_`.
    pub(super) fn transfer_image_slot(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        shape: &TransferShape,
    ) -> Result<Value, BackendError> {
        let size = u32::try_from(shape.image_size().max(8))
            .map_err(|_| BackendError::new("transfer image is too large"))?;
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size.next_multiple_of(8),
            3,
        ));
        let zero = builder.ins().iconst(types::I64, 0);
        for offset in (0..size.next_multiple_of(8)).step_by(8) {
            builder.ins().stack_store(
                zero,
                slot,
                i32::try_from(offset)
                    .map_err(|_| BackendError::new("transfer image is too large"))?,
            );
        }
        Ok(builder.ins().stack_addr(self.pointer_type, slot, 0))
    }

    /// Load the pointer-typed result of a materialized image.
    pub(super) fn load_image_pointer(
        &self,
        builder: &mut FunctionBuilder<'_>,
        image: Value,
    ) -> Value {
        builder
            .ins()
            .load(self.pointer_type, MemFlags::new(), image, 0)
    }
}
