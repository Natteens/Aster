//! Cranelift JIT backend for validated Aster MIR.
//!
//! This crate depends only on `aster-mir` from the Aster compiler pipeline and
//! on `aster-runtime` for the execution ABI. It does not inspect syntax, AST,
//! or HIR, and it exposes no Cranelift types to other crates.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use aster_mir as mir;
use aster_runtime::{RuntimeType, runtime_functions};
use aster_types::Primitive;
use cranelift_codegen::ir::{
    AbiParam, Block, InstBuilder, MemFlags, Signature, StackSlot, StackSlotData, StackSlotKind,
    TrapCode, Type as ClifType, Value,
    condcodes::{FloatCC, IntCC},
    types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, default_libcall_names};

#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionValue {
    SByte(i8),
    Byte(u8),
    Short(i16),
    UShort(u16),
    Int(i32),
    UInt(u32),
    Long(i64),
    ULong(u64),
    Float(f32),
    Double(f64),
    Bool(bool),
    Char(char),
    String(String),
    Void,
}

impl ExecutionValue {
    #[must_use]
    pub fn float(value: f32) -> Self {
        Self::Float(value)
    }

    #[must_use]
    pub fn double(value: f64) -> Self {
        Self::Double(value)
    }
}

impl fmt::Display for ExecutionValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SByte(value) => write!(formatter, "{value}"),
            Self::Byte(value) => write!(formatter, "{value}"),
            Self::Short(value) => write!(formatter, "{value}"),
            Self::UShort(value) => write!(formatter, "{value}"),
            Self::Int(value) => write!(formatter, "{value}"),
            Self::UInt(value) => write!(formatter, "{value}"),
            Self::Long(value) => write!(formatter, "{value}"),
            Self::ULong(value) => write!(formatter, "{value}"),
            Self::Float(value) => write!(formatter, "{value}"),
            Self::Double(value) => write!(formatter, "{value}"),
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Char(value) => write!(formatter, "{value}"),
            Self::String(value) => formatter.write_str(value),
            Self::Void => formatter.write_str("function completed successfully (void)"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError {
    message: String,
}

impl BackendError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for BackendError {}

/// Compile a validated MIR module in memory and invoke one explicitly selected function.
///
/// # Errors
///
/// Returns a controlled error for an invalid entry selection, unsupported MIR, or a
/// Cranelift declaration/compilation/finalization failure.
pub fn execute(module: &mir::Module, function_name: &str) -> Result<ExecutionValue, BackendError> {
    validate_module(module)?;
    let entry = select_entry(module, function_name)?;
    execute_resolved(module, entry)
}

/// Compile validated MIR and invoke the concrete function selected by the
/// compiler's application-entry layer.
///
/// # Errors
///
/// Returns a controlled error if the symbol is missing or cannot use the
/// zero-parameter host invocation ABI.
pub fn execute_symbol(
    module: &mir::Module,
    symbol: mir::SymbolId,
) -> Result<ExecutionValue, BackendError> {
    validate_module(module)?;
    let entry = module
        .functions
        .iter()
        .find(|function| function.symbol == symbol)
        .ok_or_else(|| BackendError::new(format!("entry symbol {symbol:?} was not found")))?;
    validate_invocable_entry(entry, &entry.name)?;
    execute_resolved(module, entry)
}

fn execute_resolved(
    module: &mir::Module,
    entry: &mir::Function,
) -> Result<ExecutionValue, BackendError> {
    let mut builder = JITBuilder::new(default_libcall_names()).map_err(module_error)?;
    for function in runtime_functions() {
        builder.symbol(function.name, function.address);
    }
    let jit = JITModule::new(builder);
    let mut codegen = Codegen::new(jit, module)?;
    let function_ids = codegen.declare_functions(module)?;
    codegen.define_interface_tables(module, &function_ids)?;
    for function in &module.functions {
        codegen.define_function(function, &function_ids)?;
    }
    codegen.jit.finalize_definitions().map_err(module_error)?;
    let entry_id = function_ids[&entry.symbol];
    let pointer = codegen.jit.get_finalized_function(entry_id);
    // The JITModule stays alive until after the invocation copies any result
    // (including string payloads) into host-owned memory.
    let mut execution_context = aster_runtime::ExecutionContext::new();
    let value = invoke_finalized(pointer, &entry.return_type, &mut execution_context);
    let runtime_error = execution_context.take_error();
    // SAFETY: execution finished and `value` owns copies of any data that
    // lived in the module, so no pointer into the JIT memory survives.
    // Releasing here prevents leaking code and data pages across repeated
    // executions (e.g. `aster watch` rebuilds).
    #[allow(unsafe_code)]
    unsafe {
        codegen.jit.free_memory();
    }
    if let Some(error) = runtime_error {
        Err(BackendError::new(format!("Aster runtime error: {error}")))
    } else {
        value
    }
}

/// Per-module code generation state: the JIT module, interned string literals,
/// and the imported runtime functions.
struct Codegen {
    jit: JITModule,
    pointer_type: ClifType,
    string_data: HashMap<String, DataId>,
    runtime_ids: HashMap<&'static str, FuncId>,
    interface_tables: HashMap<(mir::SymbolId, mir::SymbolId), DataId>,
    interface_methods:
        HashMap<mir::SymbolId, (mir::SymbolId, usize, mir::InterfaceMethodDefinition)>,
    layouts: Layouts,
}

#[derive(Clone, Debug)]
struct FieldLayout {
    offset: u32,
    type_: mir::Type,
}

#[derive(Clone, Debug)]
struct TypeLayout {
    size: u32,
    align_shift: u8,
}

struct Layouts {
    pointer_bytes: u32,
    structs: HashMap<mir::SymbolId, mir::StructDefinition>,
    enums: HashMap<mir::SymbolId, mir::EnumDefinition>,
    types: HashMap<mir::SymbolId, TypeLayout>,
    fields: HashMap<mir::SymbolId, FieldLayout>,
}

impl Layouts {
    fn new(module: &mir::Module, pointer_bytes: u32) -> Result<Self, BackendError> {
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
        if matches!(type_, mir::Type::Array(_) | mir::Type::Class(_)) {
            return Ok(TypeLayout {
                size: self.pointer_bytes,
                align_shift: u8::try_from(self.pointer_bytes.trailing_zeros())
                    .map_err(|_| BackendError::new("pointer alignment is too large"))?,
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

    fn type_layout(&self, type_: &mir::Type) -> Result<TypeLayout, BackendError> {
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
        if matches!(type_, mir::Type::Array(_) | mir::Type::Class(_)) {
            return Ok(TypeLayout {
                size: self.pointer_bytes,
                align_shift: u8::try_from(self.pointer_bytes.trailing_zeros())
                    .map_err(|_| BackendError::new("pointer alignment is too large"))?,
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

    fn zero_initializable(&self, type_: &mir::Type) -> bool {
        match type_ {
            mir::Type::String
            | mir::Type::Decimal
            | mir::Type::Array(_)
            | mir::Type::Class(_)
            | mir::Type::Interface(_)
            | mir::Type::Enum(_)
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

struct FunctionState {
    slots: HashMap<mir::LocalId, StackSlot>,
    execution_context: Option<Value>,
    hidden_return: Option<Value>,
}

impl Codegen {
    fn new(mut jit: JITModule, module: &mir::Module) -> Result<Self, BackendError> {
        let pointer_type = jit.target_config().pointer_type();
        let pointer_bytes = pointer_type.bytes();
        let mut runtime_ids = HashMap::new();
        for function in runtime_functions() {
            let mut signature = jit.make_signature();
            for parameter in function.signature.parameters {
                signature
                    .params
                    .push(AbiParam::new(runtime_type(*parameter, pointer_type)));
            }
            if let Some(result) = function.signature.result {
                signature
                    .returns
                    .push(AbiParam::new(runtime_type(result, pointer_type)));
            }
            let id = jit
                .declare_function(function.name, Linkage::Import, &signature)
                .map_err(module_error)?;
            runtime_ids.insert(function.name, id);
        }
        Ok(Self {
            jit,
            pointer_type,
            string_data: HashMap::new(),
            runtime_ids,
            interface_tables: HashMap::new(),
            interface_methods: module
                .interfaces
                .iter()
                .flat_map(|interface| {
                    interface
                        .methods
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(slot, method)| (method.symbol, (interface.symbol, slot, method)))
                })
                .collect(),
            layouts: Layouts::new(module, pointer_bytes)?,
        })
    }

    fn declare_functions(
        &mut self,
        mir_module: &mir::Module,
    ) -> Result<HashMap<mir::SymbolId, FuncId>, BackendError> {
        let mut functions = HashMap::new();
        for function in &mir_module.functions {
            let signature = self.signature(function)?;
            let linkage = if function.visibility == mir::Visibility::Public {
                Linkage::Export
            } else {
                Linkage::Local
            };
            let name = format!("aster_{}_{}", function.symbol.0, function.name);
            let id = self
                .jit
                .declare_function(&name, linkage, &signature)
                .map_err(module_error)?;
            functions.insert(function.symbol, id);
        }
        Ok(functions)
    }

    fn define_interface_tables(
        &mut self,
        module: &mir::Module,
        function_ids: &HashMap<mir::SymbolId, FuncId>,
    ) -> Result<(), BackendError> {
        for implementation in &module.interface_implementations {
            let id = self
                .jit
                .declare_anonymous_data(false, false)
                .map_err(module_error)?;
            let mut description = DataDescription::new();
            let size = usize::try_from(self.pointer_type.bytes())
                .map_err(|_| BackendError::new("pointer size is not addressable"))?
                .checked_mul(implementation.methods.len())
                .ok_or_else(|| BackendError::new("interface table is too large"))?;
            description.define_zeroinit(size);
            description.set_align(u64::from(self.pointer_type.bytes()));
            for (slot, method) in implementation.methods.iter().enumerate() {
                let function_id = function_ids.get(method).copied().ok_or_else(|| {
                    BackendError::new("interface table references an unknown concrete method")
                })?;
                let function = self.jit.declare_func_in_data(function_id, &mut description);
                let offset = u32::try_from(slot)
                    .ok()
                    .and_then(|slot| slot.checked_mul(self.pointer_type.bytes()))
                    .ok_or_else(|| BackendError::new("interface table offset is too large"))?;
                description.write_function_addr(offset, function);
            }
            self.jit
                .define_data(id, &description)
                .map_err(module_error)?;
            self.interface_tables
                .insert((implementation.class, implementation.interface), id);
        }
        Ok(())
    }

    fn signature(&self, function: &mir::Function) -> Result<Signature, BackendError> {
        let mut signature = self.jit.make_signature();
        // Every Aster function receives the host-owned context first. Internal
        // calls forward the same pointer; source-level signatures stay unchanged.
        signature.params.push(AbiParam::new(self.pointer_type));
        if is_aggregate(&function.return_type) {
            signature.params.push(AbiParam::new(self.pointer_type));
        }
        for parameter in &function.parameters {
            let type_ = if is_aggregate(&parameter.type_)
                || matches!(parameter.type_, mir::Type::Array(_) | mir::Type::Class(_))
            {
                self.pointer_type
            } else {
                self.clif_value_type(&parameter.type_)?
            };
            signature.params.push(AbiParam::new(type_));
        }
        if function.return_type != mir::Type::Void && !is_aggregate(&function.return_type) {
            signature
                .returns
                .push(AbiParam::new(self.clif_value_type(&function.return_type)?));
        }
        Ok(signature)
    }

    fn interface_signature(
        &self,
        method: &mir::InterfaceMethodDefinition,
    ) -> Result<Signature, BackendError> {
        let mut signature = self.jit.make_signature();
        signature.params.push(AbiParam::new(self.pointer_type));
        if is_aggregate(&method.return_type) {
            signature.params.push(AbiParam::new(self.pointer_type));
        }
        signature.params.push(AbiParam::new(self.pointer_type));
        for parameter in &method.parameters {
            let type_ = if is_aggregate(parameter)
                || matches!(parameter, mir::Type::Array(_) | mir::Type::Class(_))
            {
                self.pointer_type
            } else {
                self.clif_value_type(parameter)?
            };
            signature.params.push(AbiParam::new(type_));
        }
        if method.return_type != mir::Type::Void && !is_aggregate(&method.return_type) {
            signature
                .returns
                .push(AbiParam::new(self.clif_value_type(&method.return_type)?));
        }
        Ok(signature)
    }

    fn define_function(
        &mut self,
        function: &mir::Function,
        function_ids: &HashMap<mir::SymbolId, FuncId>,
    ) -> Result<(), BackendError> {
        let mut context = self.jit.make_context();
        context.func.signature = self.signature(function)?;
        let mut builder_context = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
            self.translate_function(&mut builder, function, function_ids)?;
            builder.seal_all_blocks();
            builder.finalize();
        }
        self.jit
            .define_function(function_ids[&function.symbol], &mut context)
            .map_err(module_error)?;
        self.jit.clear_context(&mut context);
        Ok(())
    }

    fn translate_function(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        function: &mir::Function,
        function_ids: &HashMap<mir::SymbolId, FuncId>,
    ) -> Result<(), BackendError> {
        let blocks = function
            .blocks
            .iter()
            .map(|block| (block.id, builder.create_block()))
            .collect::<HashMap<_, _>>();
        let mut state = FunctionState {
            slots: HashMap::new(),
            execution_context: None,
            hidden_return: None,
        };
        for local in function.parameters.iter().chain(&function.locals) {
            let layout = self.layouts.type_layout(&local.type_)?;
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                layout.size,
                layout.align_shift,
            ));
            state.slots.insert(local.id, slot);
        }
        let entry = blocks[&function.entry];
        builder.append_block_params_for_function_params(entry);

        for block in &function.blocks {
            let clif_block = blocks[&block.id];
            builder.switch_to_block(clif_block);
            if block.id == function.entry {
                let values = builder.block_params(entry).to_vec();
                let mut values = values.into_iter();
                state.execution_context = values.next();
                if is_aggregate(&function.return_type) {
                    state.hidden_return = values.next();
                }
                for parameter in &function.parameters {
                    let value = values.next().ok_or_else(|| {
                        BackendError::new("missing Cranelift parameter for Aster function")
                    })?;
                    let destination =
                        self.place_address(builder, &mir::Place::Local(parameter.id), &state)?;
                    if is_aggregate(&parameter.type_) {
                        self.copy_value(builder, &parameter.type_, value, destination)?;
                    } else {
                        builder.ins().store(MemFlags::new(), value, destination, 0);
                    }
                }
            }
            for instruction in &block.instructions {
                self.translate_instruction(builder, instruction, function_ids, &state)?;
            }
            self.translate_terminator(builder, &block.terminator, &blocks, &state)?;
        }
        Ok(())
    }

    fn translate_instruction(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        instruction: &mir::Instruction,
        function_ids: &HashMap<mir::SymbolId, FuncId>,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        match instruction {
            mir::Instruction::Assign { target, value } => {
                self.assign_rvalue(builder, target, value, state)
            }
            mir::Instruction::Call {
                destination,
                function,
                arguments,
                return_type,
            } => {
                let function_ref = self
                    .jit
                    .declare_func_in_func(function_ids[function], builder.func);
                let mut values = vec![state.execution_context.ok_or_else(|| {
                    BackendError::new("function is missing its ExecutionContext")
                })?];
                if is_aggregate(return_type) {
                    let destination = destination.as_ref().ok_or_else(|| {
                        BackendError::new("struct call result requires a destination")
                    })?;
                    values.push(self.place_address(builder, destination, state)?);
                }
                for argument in arguments {
                    values.push(self.translate_operand(builder, argument, state)?);
                }
                let call = builder.ins().call(function_ref, &values);
                if let Some(destination) = destination
                    && !is_aggregate(return_type)
                {
                    let result = builder.inst_results(call).first().copied().ok_or_else(|| {
                        BackendError::new("Cranelift call did not produce its declared result")
                    })?;
                    self.store_scalar(builder, destination, result, state)?;
                }
                Ok(())
            }
            call @ mir::Instruction::CallInterface { .. } => {
                self.translate_interface_call(builder, call, state)
            }
            mir::Instruction::CallIntrinsic {
                destination,
                intrinsic,
                arguments,
                ..
            } => self.translate_intrinsic(
                builder,
                destination.as_ref(),
                *intrinsic,
                arguments,
                state,
            ),
            mir::Instruction::AllocateArray {
                destination,
                element_type,
                length,
                requires_default,
            } => {
                if *requires_default && !self.layouts.zero_initializable(element_type) {
                    return Err(BackendError::new(format!(
                        "`new {}[length]` has no safe all-zero default; initialize every element with an array literal",
                        type_name(element_type)
                    )));
                }
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids["aster_rt_array_new"], builder.func);
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("array allocation is missing its ExecutionContext")
                })?;
                let length = self.translate_operand(builder, length, state)?;
                let size = i64::from(self.layouts.type_layout(element_type)?.size);
                let size = builder.ins().iconst(types::I32, size);
                let call = builder.ins().call(function_ref, &[context, length, size]);
                let array = builder.inst_results(call)[0];
                self.store_scalar(builder, destination, array, state)
            }
            mir::Instruction::AllocateObject { destination, class } => {
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids["aster_rt_object_new"], builder.func);
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("object allocation is missing its ExecutionContext")
                })?;
                let layout = self
                    .layouts
                    .types
                    .get(class)
                    .ok_or_else(|| BackendError::new("class has no computed object layout"))?;
                let size = builder.ins().iconst(types::I32, i64::from(layout.size));
                let call = builder.ins().call(function_ref, &[context, size]);
                let object = builder.inst_results(call)[0];
                self.store_scalar(builder, destination, object, state)
            }
        }
    }

    fn translate_interface_call(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        call: &mir::Instruction,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let mir::Instruction::CallInterface {
            destination,
            receiver,
            method,
            arguments,
            return_type,
        } = call
        else {
            unreachable!("interface call translator received another instruction")
        };
        let (_, slot, definition) = self
            .interface_methods
            .get(method)
            .cloned()
            .ok_or_else(|| BackendError::new("unknown interface method in MIR"))?;
        let interface = self.translate_operand(builder, receiver, state)?;
        let object = builder
            .ins()
            .load(self.pointer_type, MemFlags::new(), interface, 0);
        let pointer_bytes = i32::try_from(self.pointer_type.bytes())
            .map_err(|_| BackendError::new("pointer offset is too large"))?;
        let table =
            builder
                .ins()
                .load(self.pointer_type, MemFlags::new(), interface, pointer_bytes);
        let method_offset = i32::try_from(slot)
            .ok()
            .and_then(|slot| slot.checked_mul(pointer_bytes))
            .ok_or_else(|| BackendError::new("interface method offset is too large"))?;
        let method_pointer =
            builder
                .ins()
                .load(self.pointer_type, MemFlags::new(), table, method_offset);
        let mut values =
            vec![state.execution_context.ok_or_else(|| {
                BackendError::new("interface call is missing its ExecutionContext")
            })?];
        if is_aggregate(return_type) {
            let destination = destination.as_ref().ok_or_else(|| {
                BackendError::new("aggregate interface call requires a destination")
            })?;
            values.push(self.place_address(builder, destination, state)?);
        }
        values.push(object);
        for argument in arguments {
            values.push(self.translate_operand(builder, argument, state)?);
        }
        let signature = builder.import_signature(self.interface_signature(&definition)?);
        let call = builder
            .ins()
            .call_indirect(signature, method_pointer, &values);
        if let Some(destination) = destination.as_ref()
            && !is_aggregate(return_type)
        {
            let result =
                builder.inst_results(call).first().copied().ok_or_else(|| {
                    BackendError::new("indirect interface call produced no result")
                })?;
            self.store_scalar(builder, destination, result, state)?;
        }
        Ok(())
    }

    /// Map one MIR intrinsic onto its `aster-runtime` symbol. The log variants
    /// share `aster_rt_log` and prepend their severity as the first argument.
    fn translate_intrinsic(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        destination: Option<&mir::Place>,
        intrinsic: mir::Intrinsic,
        arguments: &[mir::Operand],
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let (symbol, immediate, needs_context) = match intrinsic {
            mir::Intrinsic::Log => ("aster_rt_log", Some(0_i64), false),
            mir::Intrinsic::LogWarning => ("aster_rt_log", Some(1), false),
            mir::Intrinsic::LogError => ("aster_rt_log", Some(2), false),
            mir::Intrinsic::StringEquals => ("aster_rt_string_eq", None, false),
            mir::Intrinsic::StringConcat => ("aster_rt_string_concat", None, true),
            mir::Intrinsic::StringLength => ("aster_rt_string_length", None, true),
            mir::Intrinsic::ReportRuntimeError(kind) => (
                "aster_rt_math_domain_error",
                Some(match kind {
                    mir::RuntimeErrorKind::MathAbsIntOverflow => 0,
                    mir::RuntimeErrorKind::MathAbsLongOverflow => 1,
                    mir::RuntimeErrorKind::MathClampInvalidRange => 2,
                }),
                true,
            ),
        };
        let function_ref = self
            .jit
            .declare_func_in_func(self.runtime_ids[symbol], builder.func);
        let mut values = Vec::with_capacity(arguments.len() + 1);
        if needs_context {
            values.push(state.execution_context.ok_or_else(|| {
                BackendError::new("runtime intrinsic requires an execution context")
            })?);
        }
        if let Some(immediate) = immediate {
            values.push(builder.ins().iconst(types::I32, immediate));
        }
        for argument in arguments {
            values.push(self.translate_operand(builder, argument, state)?);
        }
        let call = builder.ins().call(function_ref, &values);
        if let Some(destination) = destination {
            let result = builder.inst_results(call).first().copied().ok_or_else(|| {
                BackendError::new("runtime intrinsic did not produce its declared result")
            })?;
            self.store_scalar(builder, destination, result, state)?;
        }
        Ok(())
    }

    fn assign_rvalue(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        target: &mir::Place,
        value: &mir::Rvalue,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        if !is_aggregate(&value.type_) {
            let value = self.translate_rvalue(builder, value, state)?;
            return self.store_scalar(builder, target, value, state);
        }
        let destination = self.place_address(builder, target, state)?;
        match &value.kind {
            mir::RvalueKind::Use(operand) => {
                let source = self.translate_operand(builder, operand, state)?;
                self.copy_value(builder, &value.type_, source, destination)
            }
            mir::RvalueKind::Aggregate(fields) => {
                for field in fields {
                    let layout = self
                        .layouts
                        .fields
                        .get(&field.field)
                        .cloned()
                        .ok_or_else(|| BackendError::new("unknown struct field in MIR"))?;
                    let address = if layout.offset == 0 {
                        destination
                    } else {
                        builder
                            .ins()
                            .iadd_imm(destination, i64::from(layout.offset))
                    };
                    self.assign_operand_to_address(
                        builder,
                        &layout.type_,
                        &field.value,
                        address,
                        state,
                    )?;
                }
                Ok(())
            }
            mir::RvalueKind::EnumConstruct { tag, fields, .. } => {
                let tag = builder.ins().iconst(types::I32, i64::from(*tag));
                builder.ins().store(MemFlags::new(), tag, destination, 0);
                for field in fields {
                    let layout = self
                        .layouts
                        .fields
                        .get(&field.field)
                        .cloned()
                        .ok_or_else(|| BackendError::new("unknown enum payload field in MIR"))?;
                    let address = builder
                        .ins()
                        .iadd_imm(destination, i64::from(layout.offset));
                    self.assign_operand_to_address(
                        builder,
                        &layout.type_,
                        &field.value,
                        address,
                        state,
                    )?;
                }
                Ok(())
            }
            mir::RvalueKind::MakeInterface {
                object,
                class,
                interface,
            } => {
                let object = self.translate_operand(builder, object, state)?;
                builder.ins().store(MemFlags::new(), object, destination, 0);
                let table = self
                    .interface_tables
                    .get(&(*class, *interface))
                    .copied()
                    .ok_or_else(|| BackendError::new("interface conversion has no method table"))?;
                let global = self.jit.declare_data_in_func(table, builder.func);
                let table = builder.ins().global_value(self.pointer_type, global);
                builder.ins().store(
                    MemFlags::new(),
                    table,
                    destination,
                    i32::try_from(self.pointer_type.bytes())
                        .map_err(|_| BackendError::new("pointer offset is too large"))?,
                );
                Ok(())
            }
            _ => Err(BackendError::new(
                "aggregate values only support construction and value copies",
            )),
        }
    }

    fn assign_operand_to_address(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        type_: &mir::Type,
        operand: &mir::Operand,
        destination: Value,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let value = self.translate_operand(builder, operand, state)?;
        if is_aggregate(type_) {
            self.copy_value(builder, type_, value, destination)
        } else {
            builder.ins().store(MemFlags::new(), value, destination, 0);
            Ok(())
        }
    }

    fn copy_value(
        &self,
        builder: &mut FunctionBuilder<'_>,
        type_: &mir::Type,
        source: Value,
        destination: Value,
    ) -> Result<(), BackendError> {
        if matches!(type_, mir::Type::Interface(_)) {
            for offset in [0_i64, i64::from(self.pointer_type.bytes())] {
                let source = if offset == 0 {
                    source
                } else {
                    builder.ins().iadd_imm(source, offset)
                };
                let destination = if offset == 0 {
                    destination
                } else {
                    builder.ins().iadd_imm(destination, offset)
                };
                let value = builder
                    .ins()
                    .load(self.pointer_type, MemFlags::new(), source, 0);
                builder.ins().store(MemFlags::new(), value, destination, 0);
            }
            return Ok(());
        }
        if let mir::Type::Enum(symbol) = type_ {
            let definition = self
                .layouts
                .enums
                .get(symbol)
                .cloned()
                .ok_or_else(|| BackendError::new("unknown enum type in MIR"))?;
            let tag = builder.ins().load(types::I32, MemFlags::new(), source, 0);
            builder.ins().store(MemFlags::new(), tag, destination, 0);
            let join = builder.create_block();
            for case in definition.cases {
                let copy = builder.create_block();
                let next = builder.create_block();
                let active = builder
                    .ins()
                    .icmp_imm(IntCC::Equal, tag, i64::from(case.tag));
                builder.ins().brif(active, copy, &[], next, &[]);
                builder.switch_to_block(copy);
                for field in case.fields {
                    let layout = self.layouts.fields[&field.symbol].clone();
                    let left = builder.ins().iadd_imm(source, i64::from(layout.offset));
                    let right = builder
                        .ins()
                        .iadd_imm(destination, i64::from(layout.offset));
                    self.copy_value(builder, &field.type_, left, right)?;
                }
                builder.ins().jump(join, &[]);
                builder.switch_to_block(next);
            }
            builder.ins().jump(join, &[]);
            builder.switch_to_block(join);
            return Ok(());
        }
        let mir::Type::User(symbol) = type_ else {
            let clif_type = self.clif_value_type(type_)?;
            let value = builder.ins().load(clif_type, MemFlags::new(), source, 0);
            builder.ins().store(MemFlags::new(), value, destination, 0);
            return Ok(());
        };
        let definition = self
            .layouts
            .structs
            .get(symbol)
            .cloned()
            .ok_or_else(|| BackendError::new("unknown struct type in MIR"))?;
        for field in definition.fields {
            let layout =
                self.layouts.fields.get(&field.symbol).ok_or_else(|| {
                    BackendError::new("struct field has no computed runtime layout")
                })?;
            let (source, destination) = if layout.offset == 0 {
                (source, destination)
            } else {
                (
                    builder.ins().iadd_imm(source, i64::from(layout.offset)),
                    builder
                        .ins()
                        .iadd_imm(destination, i64::from(layout.offset)),
                )
            };
            self.copy_value(builder, &field.type_, source, destination)?;
        }
        Ok(())
    }

    fn place_address(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        place: &mir::Place,
        state: &FunctionState,
    ) -> Result<Value, BackendError> {
        match place {
            mir::Place::Local(local) => {
                let slot = state
                    .slots
                    .get(local)
                    .copied()
                    .ok_or_else(|| BackendError::new("MIR references an unknown local"))?;
                Ok(builder.ins().stack_addr(self.pointer_type, slot, 0))
            }
            mir::Place::Field { base, field } => {
                let base = self.place_address(builder, base, state)?;
                let field =
                    self.layouts.fields.get(field).ok_or_else(|| {
                        BackendError::new("MIR references an unknown struct field")
                    })?;
                if field.offset == 0 {
                    Ok(base)
                } else {
                    Ok(builder.ins().iadd_imm(base, i64::from(field.offset)))
                }
            }
            mir::Place::EnumField { base, field, .. } => {
                let base = self.place_address(builder, base, state)?;
                let field = self
                    .layouts
                    .fields
                    .get(field)
                    .ok_or_else(|| BackendError::new("MIR references an unknown enum field"))?;
                Ok(builder.ins().iadd_imm(base, i64::from(field.offset)))
            }
            mir::Place::Index { array, index, .. } => {
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids["aster_rt_array_element"], builder.func);
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("array access is missing its ExecutionContext")
                })?;
                let array = self.translate_operand(builder, array, state)?;
                let index = self.translate_operand(builder, index, state)?;
                let call = builder.ins().call(function_ref, &[context, array, index]);
                Ok(builder.inst_results(call)[0])
            }
            mir::Place::ObjectField { object, field } => {
                let object = self.translate_operand(builder, object, state)?;
                let field =
                    self.layouts.fields.get(field).ok_or_else(|| {
                        BackendError::new("MIR references an unknown object field")
                    })?;
                if field.offset == 0 {
                    Ok(object)
                } else {
                    Ok(builder.ins().iadd_imm(object, i64::from(field.offset)))
                }
            }
            mir::Place::Symbol(_) => Err(BackendError::new(
                "module storage is not executable in the current JIT",
            )),
        }
    }

    fn store_scalar(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        place: &mir::Place,
        value: Value,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        let address = self.place_address(builder, place, state)?;
        builder.ins().store(MemFlags::new(), value, address, 0);
        Ok(())
    }

    fn translate_rvalue(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        value: &mir::Rvalue,
        state: &FunctionState,
    ) -> Result<Value, BackendError> {
        match &value.kind {
            mir::RvalueKind::Use(operand) => self.translate_operand(builder, operand, state),
            mir::RvalueKind::Aggregate(_) => Err(BackendError::new(
                "aggregate rvalue reached scalar translation",
            )),
            mir::RvalueKind::EnumConstruct { .. } => Err(BackendError::new(
                "enum construction reached scalar translation",
            )),
            mir::RvalueKind::Discriminant(operand) => {
                let address = self.translate_operand(builder, operand, state)?;
                Ok(builder.ins().load(types::I32, MemFlags::new(), address, 0))
            }
            mir::RvalueKind::MakeInterface { .. } => Err(BackendError::new(
                "interface conversion reached scalar translation",
            )),
            mir::RvalueKind::ArrayLength(array) => {
                let function_ref = self
                    .jit
                    .declare_func_in_func(self.runtime_ids["aster_rt_array_length"], builder.func);
                let context = state.execution_context.ok_or_else(|| {
                    BackendError::new("array length is missing its ExecutionContext")
                })?;
                let array = self.translate_operand(builder, array, state)?;
                let call = builder.ins().call(function_ref, &[context, array]);
                Ok(builder.inst_results(call)[0])
            }
            mir::RvalueKind::Cast(operand) => {
                let source = operand.type_.clone();
                let operand = self.translate_operand(builder, operand, state)?;
                cast_value(builder, &source, &value.type_, operand)
            }
            mir::RvalueKind::Unary { operator, operand } => {
                let is_float = matches!(operand.type_, mir::Type::Float | mir::Type::Double);
                let operand = self.translate_operand(builder, operand, state)?;
                Ok(match operator {
                    mir::UnaryOperator::Not => builder.ins().icmp_imm(IntCC::Equal, operand, 0),
                    mir::UnaryOperator::Negate if is_float => builder.ins().fneg(operand),
                    mir::UnaryOperator::Negate => builder.ins().ineg(operand),
                })
            }
            mir::RvalueKind::Binary {
                left,
                operator,
                right,
            } => {
                let operand_type = left.type_.clone();
                let left = self.translate_operand(builder, left, state)?;
                let right = self.translate_operand(builder, right, state)?;
                translate_binary(builder, *operator, &operand_type, left, right)
            }
            mir::RvalueKind::Equality {
                left,
                right,
                negated,
            } => {
                let left_address = self.translate_operand(builder, left, state)?;
                let right_address = self.translate_operand(builder, right, state)?;
                let equal =
                    self.compare_value_at(builder, &left.type_, left_address, right_address)?;
                Ok(if *negated {
                    builder.ins().icmp_imm(IntCC::Equal, equal, 0)
                } else {
                    equal
                })
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn compare_value_at(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        type_: &mir::Type,
        left_address: Value,
        right_address: Value,
    ) -> Result<Value, BackendError> {
        match type_ {
            mir::Type::Interface(_) => {
                let left = builder
                    .ins()
                    .load(self.pointer_type, MemFlags::new(), left_address, 0);
                let right =
                    builder
                        .ins()
                        .load(self.pointer_type, MemFlags::new(), right_address, 0);
                Ok(builder.ins().icmp(IntCC::Equal, left, right))
            }
            mir::Type::User(symbol) => {
                let definition = self.layouts.structs.get(symbol).cloned().ok_or_else(|| {
                    BackendError::new("struct equality references an unknown layout")
                })?;
                let mut result = builder.ins().iconst(types::I8, 1);
                for field in definition.fields {
                    let layout = self.layouts.fields[&field.symbol].clone();
                    let left = if layout.offset == 0 {
                        left_address
                    } else {
                        builder
                            .ins()
                            .iadd_imm(left_address, i64::from(layout.offset))
                    };
                    let right = if layout.offset == 0 {
                        right_address
                    } else {
                        builder
                            .ins()
                            .iadd_imm(right_address, i64::from(layout.offset))
                    };
                    let field_equal = self.compare_value_at(builder, &layout.type_, left, right)?;
                    result = builder.ins().band(result, field_equal);
                }
                Ok(result)
            }
            mir::Type::Enum(symbol) => {
                let definition =
                    self.layouts.enums.get(symbol).cloned().ok_or_else(|| {
                        BackendError::new("enum equality references unknown layout")
                    })?;
                let left_tag = builder
                    .ins()
                    .load(types::I32, MemFlags::new(), left_address, 0);
                let right_tag = builder
                    .ins()
                    .load(types::I32, MemFlags::new(), right_address, 0);
                let tags_equal = builder.ins().icmp(IntCC::Equal, left_tag, right_tag);
                let tag_match = builder.create_block();
                let unequal = builder.create_block();
                let join = builder.create_block();
                builder.append_block_param(join, types::I8);
                builder.ins().brif(tags_equal, tag_match, &[], unequal, &[]);
                builder.switch_to_block(unequal);
                let false_value = builder.ins().iconst(types::I8, 0);
                builder.ins().jump(join, &[false_value.into()]);
                builder.switch_to_block(tag_match);
                for case in definition.cases {
                    let compare = builder.create_block();
                    let next = builder.create_block();
                    let active =
                        builder
                            .ins()
                            .icmp_imm(IntCC::Equal, left_tag, i64::from(case.tag));
                    builder.ins().brif(active, compare, &[], next, &[]);
                    builder.switch_to_block(compare);
                    let mut result = builder.ins().iconst(types::I8, 1);
                    for field in case.fields {
                        let layout = self.layouts.fields[&field.symbol].clone();
                        let left = builder
                            .ins()
                            .iadd_imm(left_address, i64::from(layout.offset));
                        let right = builder
                            .ins()
                            .iadd_imm(right_address, i64::from(layout.offset));
                        let equal = self.compare_value_at(builder, &field.type_, left, right)?;
                        result = builder.ins().band(result, equal);
                    }
                    builder.ins().jump(join, &[result.into()]);
                    builder.switch_to_block(next);
                }
                let invalid = builder.ins().iconst(types::I8, 0);
                builder.ins().jump(join, &[invalid.into()]);
                builder.switch_to_block(join);
                Ok(builder.block_params(join)[0])
            }
            mir::Type::String => {
                let left = builder
                    .ins()
                    .load(self.pointer_type, MemFlags::new(), left_address, 0);
                let right =
                    builder
                        .ins()
                        .load(self.pointer_type, MemFlags::new(), right_address, 0);
                let function = self
                    .jit
                    .declare_func_in_func(self.runtime_ids["aster_rt_string_eq"], builder.func);
                let call = builder.ins().call(function, &[left, right]);
                Ok(builder.inst_results(call)[0])
            }
            _ => {
                let clif_type = self.clif_value_type(type_)?;
                let left = builder
                    .ins()
                    .load(clif_type, MemFlags::new(), left_address, 0);
                let right = builder
                    .ins()
                    .load(clif_type, MemFlags::new(), right_address, 0);
                translate_binary(builder, mir::BinaryOperator::Equal, type_, left, right)
            }
        }
    }

    fn translate_operand(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        operand: &mir::Operand,
        state: &FunctionState,
    ) -> Result<Value, BackendError> {
        match &operand.kind {
            mir::OperandKind::Constant(mir::Constant::Integer(value)) => {
                let clif_type = self.clif_value_type(&operand.type_)?;
                let bits = integer_constant_bits(value, &operand.type_)?;
                Ok(builder.ins().iconst(clif_type, bits))
            }
            mir::OperandKind::Constant(mir::Constant::Float(value)) => {
                if operand.type_ == mir::Type::Double {
                    let value = value.parse::<f64>().map_err(|_| {
                        BackendError::new(format!("`{value}` is not a valid `double` literal"))
                    })?;
                    Ok(builder.ins().f64const(value))
                } else {
                    let value = value.parse::<f32>().map_err(|_| {
                        BackendError::new(format!("`{value}` is not a valid `float` literal"))
                    })?;
                    Ok(builder.ins().f32const(value))
                }
            }
            mir::OperandKind::Constant(mir::Constant::Character(value)) => Ok(builder
                .ins()
                .iconst(types::I32, i64::from(u32::from(*value)))),
            mir::OperandKind::Constant(mir::Constant::Boolean(value)) => {
                Ok(builder.ins().iconst(types::I8, i64::from(*value)))
            }
            mir::OperandKind::Constant(mir::Constant::String(value)) => {
                let data = self.string_literal(value)?;
                let global = self.jit.declare_data_in_func(data, builder.func);
                Ok(builder.ins().global_value(self.pointer_type, global))
            }
            mir::OperandKind::Copy(place) => {
                let address = self.place_address(builder, place, state)?;
                if is_aggregate(&operand.type_) {
                    Ok(address)
                } else {
                    let type_ = self.clif_value_type(&operand.type_)?;
                    Ok(builder.ins().load(type_, MemFlags::new(), address, 0))
                }
            }
            _ => Err(BackendError::new(
                "unsupported operand reached Cranelift after backend validation",
            )),
        }
    }

    /// Intern one string literal as 8-byte-aligned JIT data in the runtime ABI
    /// layout. The data lives exactly as long as the JIT module.
    fn string_literal(&mut self, value: &str) -> Result<DataId, BackendError> {
        if let Some(id) = self.string_data.get(value) {
            return Ok(*id);
        }
        let id = self
            .jit
            .declare_anonymous_data(false, false)
            .map_err(module_error)?;
        let mut description = DataDescription::new();
        description.define(aster_runtime::encode_str(value).into_boxed_slice());
        description.set_align(8);
        self.jit
            .define_data(id, &description)
            .map_err(module_error)?;
        self.string_data.insert(value.to_owned(), id);
        Ok(id)
    }

    fn translate_terminator(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        terminator: &mir::Terminator,
        blocks: &HashMap<mir::BasicBlockId, Block>,
        state: &FunctionState,
    ) -> Result<(), BackendError> {
        match terminator {
            mir::Terminator::Goto(target) => {
                builder.ins().jump(blocks[target], &[]);
            }
            mir::Terminator::Branch {
                condition,
                then_block,
                else_block,
            } => {
                let condition = self.translate_operand(builder, condition, state)?;
                builder
                    .ins()
                    .brif(condition, blocks[then_block], &[], blocks[else_block], &[]);
            }
            mir::Terminator::Return(value) => {
                if let Some(value) = value
                    && is_aggregate(&value.type_)
                {
                    let source = self.translate_operand(builder, value, state)?;
                    let destination = state.hidden_return.ok_or_else(|| {
                        BackendError::new("struct return is missing its hidden destination")
                    })?;
                    self.copy_value(builder, &value.type_, source, destination)?;
                    builder.ins().return_(&[]);
                    return Ok(());
                }
                let values = value
                    .as_ref()
                    .map(|value| self.translate_operand(builder, value, state))
                    .transpose()?
                    .into_iter()
                    .collect::<Vec<_>>();
                builder.ins().return_(&values);
            }
            mir::Terminator::End => {
                builder.ins().return_(&[]);
            }
            mir::Terminator::Unreachable => {
                builder.ins().trap(TrapCode::unwrap_user(1));
            }
        }
        Ok(())
    }

    fn clif_value_type(&self, type_: &mir::Type) -> Result<ClifType, BackendError> {
        if matches!(type_, mir::Type::Array(_) | mir::Type::Class(_)) {
            return Ok(self.pointer_type);
        }
        match primitive(type_) {
            Some(Primitive::Bool | Primitive::SByte | Primitive::Byte) => Ok(types::I8),
            Some(Primitive::Short | Primitive::UShort) => Ok(types::I16),
            // `char` is an i32 Unicode scalar in this ABI.
            Some(Primitive::Char | Primitive::Int | Primitive::UInt) => Ok(types::I32),
            Some(Primitive::Long | Primitive::ULong) => Ok(types::I64),
            Some(Primitive::Float) => Ok(types::F32),
            Some(Primitive::Double) => Ok(types::F64),
            Some(Primitive::String) => Ok(self.pointer_type),
            Some(Primitive::Decimal) | None => Err(BackendError::new(format!(
                "type `{}` cannot be represented by the current JIT",
                type_name(type_)
            ))),
        }
    }
}

fn select_entry<'a>(
    module: &'a mir::Module,
    function_name: &str,
) -> Result<&'a mir::Function, BackendError> {
    let function = module
        .functions
        .iter()
        .find(|function| function.name == function_name)
        .ok_or_else(|| BackendError::new(format!("function `{function_name}` was not found")))?;
    if function.owner.is_some() {
        return Err(BackendError::new(format!(
            "function `{function_name}` is a method; classes and objects are not supported by the JIT"
        )));
    }
    validate_invocable_entry(function, function_name)?;
    Ok(function)
}

fn validate_invocable_entry(
    function: &mir::Function,
    function_name: &str,
) -> Result<(), BackendError> {
    if function.visibility != mir::Visibility::Public {
        return Err(BackendError::new(format!(
            "function `{function_name}` is not public"
        )));
    }
    if !function.parameters.is_empty() {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` must have no parameters"
        )));
    }
    if matches!(function.return_type, mir::Type::User(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns a struct; call it from a scalar entry function instead"
        )));
    }
    if matches!(function.return_type, mir::Type::Array(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns an array; call it from a scalar entry function instead"
        )));
    }
    if matches!(function.return_type, mir::Type::Class(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns an object reference; call it from a scalar entry function instead"
        )));
    }
    if matches!(function.return_type, mir::Type::Interface(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns an interface reference; call it from a scalar entry function instead"
        )));
    }
    if matches!(function.return_type, mir::Type::Enum(_)) {
        return Err(BackendError::new(format!(
            "entry function `{function_name}` returns an enum; handle it in a scalar entry function instead"
        )));
    }
    Ok(())
}

fn validate_module(module: &mir::Module) -> Result<(), BackendError> {
    let signatures = module
        .functions
        .iter()
        .map(|function| (function.symbol, function))
        .collect::<HashMap<_, _>>();
    let classes = module
        .classes
        .iter()
        .map(|class| class.symbol)
        .collect::<HashSet<_>>();
    let (interface_methods, implementations) =
        validate_interface_metadata(module, &signatures, &classes)?;
    for function in &module.functions {
        validate_function(
            function,
            &signatures,
            &classes,
            &interface_methods,
            &implementations,
        )?;
    }
    Ok(())
}

type InterfaceMethods<'a> =
    HashMap<mir::SymbolId, (mir::SymbolId, &'a mir::InterfaceMethodDefinition)>;

fn validate_interface_metadata<'a>(
    module: &'a mir::Module,
    signatures: &HashMap<mir::SymbolId, &'a mir::Function>,
    classes: &HashSet<mir::SymbolId>,
) -> Result<
    (
        InterfaceMethods<'a>,
        HashSet<(mir::SymbolId, mir::SymbolId)>,
    ),
    BackendError,
> {
    let mut methods = HashMap::new();
    let interfaces = module
        .interfaces
        .iter()
        .map(|interface| {
            for method in &interface.methods {
                methods.insert(method.symbol, (interface.symbol, method));
            }
            (interface.symbol, interface)
        })
        .collect::<HashMap<_, _>>();
    let mut implementations = HashSet::new();
    for implementation in &module.interface_implementations {
        if !classes.contains(&implementation.class) {
            return Err(BackendError::new(
                "interface implementation references an unknown class",
            ));
        }
        let interface = interfaces.get(&implementation.interface).ok_or_else(|| {
            BackendError::new("interface implementation references an unknown interface")
        })?;
        if !implementations.insert((implementation.class, implementation.interface)) {
            return Err(BackendError::new(
                "duplicate interface implementation in MIR",
            ));
        }
        if implementation.methods.len() != interface.methods.len() {
            return Err(BackendError::new(
                "interface implementation has the wrong method count",
            ));
        }
        for (required, concrete_symbol) in interface.methods.iter().zip(&implementation.methods) {
            let concrete = signatures.get(concrete_symbol).ok_or_else(|| {
                BackendError::new("interface implementation references an unknown method")
            })?;
            if concrete.owner != Some(implementation.class)
                || concrete.visibility != mir::Visibility::Public
                || concrete.return_type != required.return_type
                || concrete.parameters.first().map(|receiver| &receiver.type_)
                    != Some(&mir::Type::Class(implementation.class))
                || concrete
                    .parameters
                    .iter()
                    .skip(1)
                    .map(|parameter| &parameter.type_)
                    .ne(required.parameters.iter())
            {
                return Err(BackendError::new(format!(
                    "concrete method `{}` does not match interface method `{}`",
                    concrete.name, required.name
                )));
            }
        }
    }
    Ok((methods, implementations))
}

fn validate_function(
    function: &mir::Function,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    classes: &HashSet<mir::SymbolId>,
    interface_methods: &InterfaceMethods<'_>,
    implementations: &HashSet<(mir::SymbolId, mir::SymbolId)>,
) -> Result<(), BackendError> {
    if function
        .owner
        .is_some_and(|owner| !classes.contains(&owner))
    {
        return Err(unsupported(&function.name, "struct methods"));
    }
    validate_return_type(&function.return_type, &function.name)?;
    for parameter in &function.parameters {
        validate_value_type(&parameter.type_, &function.name)?;
    }
    for local in &function.locals {
        validate_value_type(&local.type_, &function.name)?;
    }
    for block in &function.blocks {
        for instruction in &block.instructions {
            validate_instruction(
                instruction,
                &function.name,
                signatures,
                classes,
                interface_methods,
                implementations,
            )?;
        }
        validate_terminator(&block.terminator, &function.name)?;
    }
    Ok(())
}

fn validate_instruction(
    instruction: &mir::Instruction,
    function_name: &str,
    signatures: &HashMap<mir::SymbolId, &mir::Function>,
    classes: &HashSet<mir::SymbolId>,
    interface_methods: &InterfaceMethods<'_>,
    implementations: &HashSet<(mir::SymbolId, mir::SymbolId)>,
) -> Result<(), BackendError> {
    match instruction {
        mir::Instruction::Assign { target, value } => {
            validate_place(target, function_name)?;
            validate_rvalue(value, function_name, implementations)
        }
        mir::Instruction::Call {
            destination,
            function,
            arguments,
            return_type,
        } => {
            if let Some(destination) = destination {
                validate_place(destination, function_name)?;
            }
            validate_return_type(return_type, function_name)?;
            for argument in arguments {
                validate_operand(argument, function_name)?;
            }
            let called = signatures.get(function).ok_or_else(|| {
                BackendError::new(format!(
                    "function `{function_name}` calls an unsupported external function with symbol {}",
                    function.0
                ))
            })?;
            if called.owner.is_some_and(|owner| !classes.contains(&owner)) {
                return Err(unsupported(function_name, "struct method calls"));
            }
            Ok(())
        }
        mir::Instruction::CallInterface {
            destination,
            receiver,
            arguments,
            return_type,
            method,
        } => validate_interface_call(
            destination.as_ref(),
            receiver,
            arguments,
            return_type,
            *method,
            function_name,
            interface_methods,
        ),
        mir::Instruction::CallIntrinsic {
            destination,
            intrinsic,
            arguments,
            return_type,
        } => {
            if let Some(destination) = destination {
                validate_place(destination, function_name)?;
            }
            validate_return_type(return_type, function_name)?;
            for argument in arguments {
                validate_operand(argument, function_name)?;
            }
            validate_intrinsic_shape(
                destination.as_ref(),
                *intrinsic,
                arguments,
                return_type,
                function_name,
            )?;
            Ok(())
        }
        mir::Instruction::AllocateArray {
            destination,
            element_type,
            length,
            ..
        } => {
            validate_place(destination, function_name)?;
            validate_value_type(element_type, function_name)?;
            validate_operand(length, function_name)
        }
        mir::Instruction::AllocateObject { destination, class } => {
            validate_place(destination, function_name)?;
            if classes.contains(class) {
                Ok(())
            } else {
                Err(unsupported(function_name, "allocation of a non-class type"))
            }
        }
    }
}

fn validate_interface_call(
    destination: Option<&mir::Place>,
    receiver: &mir::Operand,
    arguments: &[mir::Operand],
    return_type: &mir::Type,
    method: mir::SymbolId,
    function_name: &str,
    interface_methods: &InterfaceMethods<'_>,
) -> Result<(), BackendError> {
    if let Some(destination) = destination {
        validate_place(destination, function_name)?;
    }
    let mir::Type::Interface(receiver_interface) = receiver.type_ else {
        return Err(BackendError::new(format!(
            "function `{function_name}` has an interface call with a non-interface receiver"
        )));
    };
    validate_operand(receiver, function_name)?;
    let (method_interface, definition) = interface_methods
        .get(&method)
        .ok_or_else(|| BackendError::new("interface call references an unknown contract method"))?;
    let incompatible = *method_interface != receiver_interface
        || definition.return_type != *return_type
        || definition.parameters.len() != arguments.len()
        || definition
            .parameters
            .iter()
            .zip(arguments)
            .any(|(expected, actual)| expected != &actual.type_);
    if incompatible {
        return Err(BackendError::new(format!(
            "function `{function_name}` contains an interface call with an incompatible signature"
        )));
    }
    for argument in arguments {
        validate_operand(argument, function_name)?;
    }
    validate_return_type(return_type, function_name)
}

fn validate_intrinsic_shape(
    destination: Option<&mir::Place>,
    intrinsic: mir::Intrinsic,
    arguments: &[mir::Operand],
    return_type: &mir::Type,
    function_name: &str,
) -> Result<(), BackendError> {
    let valid = match intrinsic {
        mir::Intrinsic::Log | mir::Intrinsic::LogWarning | mir::Intrinsic::LogError => {
            destination.is_none()
                && return_type == &mir::Type::Void
                && matches!(arguments, [argument] if argument.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringEquals => {
            destination.is_some()
                && return_type == &mir::Type::Bool
                && matches!(arguments, [left, right] if left.type_ == mir::Type::String && right.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringConcat => {
            destination.is_some()
                && return_type == &mir::Type::String
                && matches!(arguments, [left, right] if left.type_ == mir::Type::String && right.type_ == mir::Type::String)
        }
        mir::Intrinsic::StringLength => {
            destination.is_some()
                && return_type == &mir::Type::Int
                && matches!(arguments, [value] if value.type_ == mir::Type::String)
        }
        mir::Intrinsic::ReportRuntimeError(_) => {
            destination.is_none() && return_type == &mir::Type::Void && arguments.is_empty()
        }
    };
    if valid {
        Ok(())
    } else {
        Err(BackendError::new(format!(
            "function `{function_name}` contains a malformed {intrinsic:?} runtime intrinsic"
        )))
    }
}

fn validate_rvalue(
    value: &mir::Rvalue,
    function_name: &str,
    implementations: &HashSet<(mir::SymbolId, mir::SymbolId)>,
) -> Result<(), BackendError> {
    validate_value_type(&value.type_, function_name)?;
    if matches!(value.type_, mir::Type::Float | mir::Type::Double)
        && matches!(
            value.kind,
            mir::RvalueKind::Binary {
                operator: mir::BinaryOperator::Remainder,
                ..
            }
        )
    {
        return Err(BackendError::new(format!(
            "floating-point remainder is not yet supported by the JIT in function `{function_name}`"
        )));
    }
    match &value.kind {
        mir::RvalueKind::Aggregate(fields) | mir::RvalueKind::EnumConstruct { fields, .. } => {
            for field in fields {
                validate_operand(&field.value, function_name)?;
            }
            Ok(())
        }
        mir::RvalueKind::ArrayLength(array) => validate_operand(array, function_name),
        mir::RvalueKind::MakeInterface {
            object,
            class,
            interface,
        } => {
            if !matches!(object.type_, mir::Type::Class(_)) {
                return Err(BackendError::new(format!(
                    "function `{function_name}` converts a non-class value to an interface"
                )));
            }
            if object.type_ != mir::Type::Class(*class)
                || value.type_ != mir::Type::Interface(*interface)
                || !implementations.contains(&(*class, *interface))
            {
                return Err(BackendError::new(format!(
                    "function `{function_name}` contains an invalid class-to-interface conversion"
                )));
            }
            validate_operand(object, function_name)
        }
        mir::RvalueKind::Discriminant(operand)
        | mir::RvalueKind::Use(operand)
        | mir::RvalueKind::Cast(operand)
        | mir::RvalueKind::Unary { operand, .. } => validate_operand(operand, function_name),
        mir::RvalueKind::Binary { left, right, .. }
        | mir::RvalueKind::Equality { left, right, .. } => {
            validate_operand(left, function_name)?;
            validate_operand(right, function_name)
        }
    }
}

fn validate_terminator(
    terminator: &mir::Terminator,
    function_name: &str,
) -> Result<(), BackendError> {
    match terminator {
        mir::Terminator::Branch { condition, .. } => validate_operand(condition, function_name),
        mir::Terminator::Return(Some(value)) => validate_operand(value, function_name),
        mir::Terminator::Goto(_)
        | mir::Terminator::Return(None)
        | mir::Terminator::End
        | mir::Terminator::Unreachable => Ok(()),
    }
}

fn validate_operand(operand: &mir::Operand, function_name: &str) -> Result<(), BackendError> {
    validate_value_type(&operand.type_, function_name)?;
    match &operand.kind {
        mir::OperandKind::Constant(mir::Constant::Integer(value)) => integer_constant_bits(
            value,
            &operand.type_,
        )
        .map(|_| ())
        .map_err(|_| {
            BackendError::new(format!(
                "integer constant `{value}` in function `{function_name}` does not fit `{}`",
                type_name(&operand.type_)
            ))
        }),
        mir::OperandKind::Constant(_) | mir::OperandKind::Copy(mir::Place::Local(_)) => Ok(()),
        mir::OperandKind::Copy(place) => validate_place(place, function_name),
        mir::OperandKind::Function(_) => Err(unsupported(function_name, "function values")),
    }
}

fn validate_place(place: &mir::Place, function_name: &str) -> Result<(), BackendError> {
    match place {
        mir::Place::Local(_) => Ok(()),
        mir::Place::Field { base, .. } | mir::Place::EnumField { base, .. } => {
            validate_place(base, function_name)
        }
        mir::Place::Index {
            array,
            index,
            element_type,
        } => {
            validate_operand(array, function_name)?;
            validate_operand(index, function_name)?;
            validate_value_type(element_type, function_name)
        }
        mir::Place::ObjectField { object, .. } => validate_operand(object, function_name),
        mir::Place::Symbol(_) => Err(unsupported(
            function_name,
            "module globals, classes, and objects",
        )),
    }
}

fn executable_value_type(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::SByte
            | mir::Type::Byte
            | mir::Type::Short
            | mir::Type::UShort
            | mir::Type::Int
            | mir::Type::UInt
            | mir::Type::Long
            | mir::Type::ULong
            | mir::Type::Float
            | mir::Type::Double
            | mir::Type::Bool
            | mir::Type::Char
            | mir::Type::String
    )
}

fn validate_value_type(type_: &mir::Type, function_name: &str) -> Result<(), BackendError> {
    if let mir::Type::Array(element) = type_ {
        if matches!(**element, mir::Type::Array(_)) {
            return Err(unsupported(function_name, "nested arrays"));
        }
        return validate_value_type(element, function_name);
    }
    if executable_value_type(type_)
        || matches!(
            type_,
            mir::Type::User(_) | mir::Type::Class(_) | mir::Type::Interface(_) | mir::Type::Enum(_)
        )
    {
        Ok(())
    } else if *type_ == mir::Type::Decimal {
        Err(BackendError::new(format!(
            "`decimal` is checked by the compiler but cannot execute yet in function `{function_name}`; a dedicated decimal runtime representation is the planned next step"
        )))
    } else {
        Err(unsupported(
            function_name,
            &format!("values of type `{}`", type_name(type_)),
        ))
    }
}

fn validate_return_type(type_: &mir::Type, function_name: &str) -> Result<(), BackendError> {
    if *type_ == mir::Type::Void {
        Ok(())
    } else {
        validate_value_type(type_, function_name)
    }
}

fn unsupported(function_name: &str, feature: &str) -> BackendError {
    BackendError::new(format!(
        "Cranelift JIT does not yet support {feature} in function `{function_name}`"
    ))
}

fn type_name(type_: &mir::Type) -> &'static str {
    primitive(type_).map_or_else(
        || match type_ {
            mir::Type::Void => "void",
            mir::Type::User(_) => "user type",
            mir::Type::Class(_) => "class",
            mir::Type::Interface(_) => "interface",
            mir::Type::Enum(_) => "enum",
            mir::Type::Array(_) => "array",
            mir::Type::Unknown => "unknown",
            _ => unreachable!("every primitive MIR type has an aster-types adapter"),
        },
        Primitive::name,
    )
}

/// The single adapter between MIR's resolved type representation and the
/// backend-neutral primitive model. Cranelift-specific representation choices
/// remain local to this crate.
fn primitive(type_: &mir::Type) -> Option<Primitive> {
    Some(match type_ {
        mir::Type::Bool => Primitive::Bool,
        mir::Type::Char => Primitive::Char,
        mir::Type::SByte => Primitive::SByte,
        mir::Type::Byte => Primitive::Byte,
        mir::Type::Short => Primitive::Short,
        mir::Type::UShort => Primitive::UShort,
        mir::Type::Int => Primitive::Int,
        mir::Type::UInt => Primitive::UInt,
        mir::Type::Long => Primitive::Long,
        mir::Type::ULong => Primitive::ULong,
        mir::Type::Float => Primitive::Float,
        mir::Type::Double => Primitive::Double,
        mir::Type::Decimal => Primitive::Decimal,
        mir::Type::String => Primitive::String,
        mir::Type::Void
        | mir::Type::User(_)
        | mir::Type::Class(_)
        | mir::Type::Interface(_)
        | mir::Type::Enum(_)
        | mir::Type::Array(_)
        | mir::Type::Unknown => {
            return None;
        }
    })
}

fn is_aggregate(type_: &mir::Type) -> bool {
    matches!(
        type_,
        mir::Type::User(_) | mir::Type::Interface(_) | mir::Type::Enum(_)
    )
}

fn translate_binary(
    builder: &mut FunctionBuilder<'_>,
    operator: mir::BinaryOperator,
    operand_type: &mir::Type,
    left: Value,
    right: Value,
) -> Result<Value, BackendError> {
    if matches!(operand_type, mir::Type::Float | mir::Type::Double) {
        return translate_float_binary(builder, operator, left, right);
    }
    let unsigned = is_unsigned_integer(operand_type);
    Ok(match operator {
        mir::BinaryOperator::Multiply => builder.ins().imul(left, right),
        mir::BinaryOperator::Divide if unsigned => builder.ins().udiv(left, right),
        mir::BinaryOperator::Divide => builder.ins().sdiv(left, right),
        mir::BinaryOperator::Remainder if unsigned => builder.ins().urem(left, right),
        mir::BinaryOperator::Remainder => builder.ins().srem(left, right),
        mir::BinaryOperator::Add => builder.ins().iadd(left, right),
        mir::BinaryOperator::Subtract => builder.ins().isub(left, right),
        mir::BinaryOperator::Less if unsigned => {
            builder.ins().icmp(IntCC::UnsignedLessThan, left, right)
        }
        mir::BinaryOperator::Less => builder.ins().icmp(IntCC::SignedLessThan, left, right),
        mir::BinaryOperator::LessEqual if unsigned => {
            builder
                .ins()
                .icmp(IntCC::UnsignedLessThanOrEqual, left, right)
        }
        mir::BinaryOperator::LessEqual => {
            builder
                .ins()
                .icmp(IntCC::SignedLessThanOrEqual, left, right)
        }
        mir::BinaryOperator::Greater if unsigned => {
            builder.ins().icmp(IntCC::UnsignedGreaterThan, left, right)
        }
        mir::BinaryOperator::Greater => builder.ins().icmp(IntCC::SignedGreaterThan, left, right),
        mir::BinaryOperator::GreaterEqual if unsigned => {
            builder
                .ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, left, right)
        }
        mir::BinaryOperator::GreaterEqual => {
            builder
                .ins()
                .icmp(IntCC::SignedGreaterThanOrEqual, left, right)
        }
        mir::BinaryOperator::Equal => builder.ins().icmp(IntCC::Equal, left, right),
        mir::BinaryOperator::NotEqual => builder.ins().icmp(IntCC::NotEqual, left, right),
        mir::BinaryOperator::LogicalAnd => builder.ins().band(left, right),
        mir::BinaryOperator::LogicalOr => builder.ins().bor(left, right),
    })
}

/// Unsigned integer types (plus `char` and `bool`, which never mix signs in
/// comparisons that reach here).
fn is_unsigned_integer(type_: &mir::Type) -> bool {
    primitive(type_)
        .is_some_and(|primitive| primitive.is_unsigned() || primitive == Primitive::Char)
}

/// Bit width of an integer-representable type. `bool` and `char` have integer
/// machine representations even though they are not arithmetic integers.
fn integer_width(type_: &mir::Type) -> Option<u16> {
    let primitive = primitive(type_)?;
    (primitive.is_integer() || matches!(primitive, Primitive::Bool | Primitive::Char))
        .then(|| primitive.bit_width())
        .flatten()
}

/// Parse an integer constant into its i64 bit representation, checking range.
#[allow(clippy::cast_possible_wrap)]
fn integer_constant_bits(text: &str, type_: &mir::Type) -> Result<i64, BackendError> {
    let bits = match type_ {
        mir::Type::SByte => text.parse::<i8>().map(i64::from).ok(),
        mir::Type::Byte => text.parse::<u8>().map(i64::from).ok(),
        mir::Type::Short => text.parse::<i16>().map(i64::from).ok(),
        mir::Type::UShort => text.parse::<u16>().map(i64::from).ok(),
        mir::Type::Int => text.parse::<i32>().map(i64::from).ok(),
        mir::Type::UInt => text.parse::<u32>().map(i64::from).ok(),
        mir::Type::Long => text.parse::<i64>().ok(),
        mir::Type::ULong => text.parse::<u64>().map(|value| value as i64).ok(),
        _ => None,
    };
    bits.ok_or_else(|| {
        BackendError::new(format!(
            "integer `{text}` is outside `{}` range",
            type_name(type_)
        ))
    })
}

/// IEEE-754 arithmetic and comparisons; comparisons are "ordered", so any
/// comparison involving NaN is false except `!=`, which is true.
fn translate_float_binary(
    builder: &mut FunctionBuilder<'_>,
    operator: mir::BinaryOperator,
    left: Value,
    right: Value,
) -> Result<Value, BackendError> {
    Ok(match operator {
        mir::BinaryOperator::Multiply => builder.ins().fmul(left, right),
        mir::BinaryOperator::Divide => builder.ins().fdiv(left, right),
        mir::BinaryOperator::Add => builder.ins().fadd(left, right),
        mir::BinaryOperator::Subtract => builder.ins().fsub(left, right),
        mir::BinaryOperator::Less => builder.ins().fcmp(FloatCC::LessThan, left, right),
        mir::BinaryOperator::LessEqual => builder.ins().fcmp(FloatCC::LessThanOrEqual, left, right),
        mir::BinaryOperator::Greater => builder.ins().fcmp(FloatCC::GreaterThan, left, right),
        mir::BinaryOperator::GreaterEqual => {
            builder.ins().fcmp(FloatCC::GreaterThanOrEqual, left, right)
        }
        mir::BinaryOperator::Equal => builder.ins().fcmp(FloatCC::Equal, left, right),
        mir::BinaryOperator::NotEqual => builder.ins().fcmp(FloatCC::NotEqual, left, right),
        mir::BinaryOperator::Remainder => {
            return Err(BackendError::new(
                "floating-point remainder is not yet supported by the JIT",
            ));
        }
        mir::BinaryOperator::LogicalAnd | mir::BinaryOperator::LogicalOr => {
            return Err(BackendError::new(
                "logical operators require boolean operands",
            ));
        }
    })
}

/// Convert a value between primitive representations. Integer width changes
/// extend by the signedness of the source (two's complement) or truncate;
/// float-to-integer casts saturate at the target range and convert NaN to
/// zero instead of trapping.
fn cast_value(
    builder: &mut FunctionBuilder<'_>,
    source: &mir::Type,
    target: &mir::Type,
    value: Value,
) -> Result<Value, BackendError> {
    use mir::Type::{Double, Float};
    let float = |type_: &mir::Type| matches!(type_, Float | Double);
    if source == target {
        return Ok(value);
    }
    Ok(match (integer_width(source), integer_width(target)) {
        (Some(from), Some(to)) => {
            let clif_target = clif_integer(to)?;
            match to.cmp(&from) {
                Ordering::Greater if is_unsigned_integer(source) => {
                    builder.ins().uextend(clif_target, value)
                }
                Ordering::Greater => builder.ins().sextend(clif_target, value),
                Ordering::Less => builder.ins().ireduce(clif_target, value),
                Ordering::Equal => value,
            }
        }
        (Some(_), None) if float(target) => {
            let clif_target = if *target == Double {
                types::F64
            } else {
                types::F32
            };
            if is_unsigned_integer(source) {
                builder.ins().fcvt_from_uint(clif_target, value)
            } else {
                builder.ins().fcvt_from_sint(clif_target, value)
            }
        }
        (None, Some(to)) if float(source) => {
            let clif_target = clif_integer(to)?;
            if is_unsigned_integer(target) {
                builder.ins().fcvt_to_uint_sat(clif_target, value)
            } else {
                builder.ins().fcvt_to_sint_sat(clif_target, value)
            }
        }
        (None, None) if *source == Float && *target == Double => {
            builder.ins().fpromote(types::F64, value)
        }
        (None, None) if *source == Double && *target == Float => {
            builder.ins().fdemote(types::F32, value)
        }
        _ => {
            return Err(BackendError::new(format!(
                "cannot convert `{}` to `{}` in the current JIT",
                type_name(source),
                type_name(target)
            )));
        }
    })
}

fn clif_integer(width: u16) -> Result<ClifType, BackendError> {
    match width {
        8 => Ok(types::I8),
        16 => Ok(types::I16),
        32 => Ok(types::I32),
        64 => Ok(types::I64),
        _ => Err(BackendError::new(format!(
            "unsupported integer width `{width}` reached Cranelift"
        ))),
    }
}

fn runtime_type(type_: RuntimeType, pointer: ClifType) -> ClifType {
    match type_ {
        RuntimeType::I8 => types::I8,
        RuntimeType::I32 => types::I32,
        RuntimeType::I64 => types::I64,
        RuntimeType::Pointer => pointer,
    }
}

fn module_error(error: impl fmt::Debug + fmt::Display) -> BackendError {
    BackendError::new(format!("Cranelift JIT error: {error}\n{error:?}"))
}

/// This is the only unsafe boundary in the backend. Cranelift returns an untyped
/// pointer after successful finalization. Validation guarantees that the selected
/// entry has no parameters and that its ABI result is exactly `i32`, `u8`, a
/// runtime-ABI string pointer, or void. The `JITModule` remains alive during the
/// call, and string payloads are copied into host memory before it is dropped.
#[allow(unsafe_code)]
fn invoke_finalized(
    pointer: *const u8,
    return_type: &mir::Type,
    context: &mut aster_runtime::ExecutionContext,
) -> Result<ExecutionValue, BackendError> {
    let value = match return_type {
        mir::Type::SByte => {
            // SAFETY: The function was declared and finalized as `() -> i8` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> i8 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::SByte(function(context))
        }
        mir::Type::Byte => {
            // SAFETY: The function was declared and finalized as `() -> i8` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u8 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Byte(function(context))
        }
        mir::Type::Short => {
            // SAFETY: The function was declared and finalized as `() -> i16` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> i16 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Short(function(context))
        }
        mir::Type::UShort => {
            // SAFETY: The function was declared and finalized as `() -> i16` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u16 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::UShort(function(context))
        }
        mir::Type::UInt => {
            // SAFETY: The function was declared and finalized as `() -> i32` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u32 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::UInt(function(context))
        }
        mir::Type::ULong => {
            // SAFETY: The function was declared and finalized as `() -> i64` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u64 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::ULong(function(context))
        }
        mir::Type::Int => {
            // SAFETY: The function was declared and finalized as `() -> i32` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> i32 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Int(function(context))
        }
        mir::Type::Long => {
            // SAFETY: The function was declared and finalized as `() -> i64` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> i64 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Long(function(context))
        }
        mir::Type::Float => {
            // SAFETY: The function was declared and finalized as `() -> f32` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> f32 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::float(function(context))
        }
        mir::Type::Double => {
            // SAFETY: The function was declared and finalized as `() -> f64` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> f64 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::double(function(context))
        }
        mir::Type::Char => {
            // SAFETY: The function was declared and finalized as `() -> i32` above,
            // holding a Unicode scalar value.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u32 =
                unsafe { std::mem::transmute(pointer) };
            let value = function(context);
            let value = char::from_u32(value).ok_or_else(|| {
                BackendError::new(format!(
                    "Aster function returned invalid Unicode scalar value U+{value:08X} as `char`"
                ))
            })?;
            ExecutionValue::Char(value)
        }
        mir::Type::Bool => {
            // SAFETY: The function was declared and finalized as `() -> i8` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) -> u8 =
                unsafe { std::mem::transmute(pointer) };
            ExecutionValue::Bool(function(context) != 0)
        }
        mir::Type::String => {
            // SAFETY: The function was declared and finalized as `() -> ptr`,
            // where the pointer follows the runtime string ABI and stays valid
            // while the JIT module is alive.
            let function: extern "C" fn(
                *mut aster_runtime::ExecutionContext,
            ) -> *const aster_runtime::AsterStrHeader = unsafe { std::mem::transmute(pointer) };
            let result = function(context);
            // SAFETY: the returned pointer originates from this module's data
            // section; the module is still alive here and `decode_str` copies
            // the payload before the module can be dropped.
            let value = unsafe { aster_runtime::decode_str(result) };
            match value {
                Some(value) => ExecutionValue::String(value),
                None => ExecutionValue::String(String::from(
                    "<invalid string returned by Aster function>",
                )),
            }
        }
        mir::Type::Void => {
            // SAFETY: The function was declared and finalized as `() -> ()` above.
            let function: extern "C" fn(*mut aster_runtime::ExecutionContext) =
                unsafe { std::mem::transmute(pointer) };
            function(context);
            ExecutionValue::Void
        }
        _ => unreachable!("entry return type was validated before code generation"),
    };
    Ok(value)
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
}
