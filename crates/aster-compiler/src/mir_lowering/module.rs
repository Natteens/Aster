use super::symbols::SymbolAllocator;
use super::{Arc, FunctionLowerer, HashMap, async_machine, hir, mir};

#[allow(clippy::too_many_lines)]
pub(super) fn lower(module: &hir::Module) -> mir::Module {
    let mut symbols = SymbolAllocator::new(module);
    let intrinsics = module
        .items
        .iter()
        .flat_map(|item| match item {
            hir::Item::Function(function) => std::slice::from_ref(function),
            hir::Item::Class(declaration) | hir::Item::Struct(declaration) => {
                declaration.methods.as_slice()
            }
            hir::Item::Interface(_) | hir::Item::Enum(_) | hir::Item::Variable(_) => &[],
        })
        .filter_map(|function| {
            function
                .intrinsic
                .map(|intrinsic| (function.symbol, intrinsic))
        })
        .collect::<HashMap<_, _>>();
    let structs = module
        .items
        .iter()
        .filter_map(|item| {
            let hir::Item::Struct(declaration) = item else {
                return None;
            };
            Some(mir::StructDefinition {
                symbol: declaration.symbol,
                name: declaration.name.clone(),
                fields: declaration
                    .fields
                    .iter()
                    .map(|field| mir::FieldDefinition {
                        symbol: field.symbol,
                        name: field.name.clone(),
                        type_: field.type_.clone(),
                    })
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    let (vtables, virtual_slots) = build_virtual_tables(module);
    let virtual_slots = Arc::new(virtual_slots);
    let classes = module
        .items
        .iter()
        .filter_map(|item| {
            let hir::Item::Class(declaration) = item else {
                return None;
            };
            Some(mir::ClassDefinition {
                base: declaration.base,
                is_abstract: declaration.is_abstract,
                is_sealed: declaration.is_sealed,
                symbol: declaration.symbol,
                name: declaration.name.clone(),
                fields: declaration
                    .fields
                    .iter()
                    .map(|field| mir::FieldDefinition {
                        symbol: field.symbol,
                        name: field.name.clone(),
                        type_: field.type_.clone(),
                    })
                    .collect(),
                vtable: vtables
                    .get(&declaration.symbol)
                    .cloned()
                    .unwrap_or_default(),
            })
        })
        .collect();
    let (interfaces, interface_implementations, thunks) =
        lower_interfaces(module, &vtables, &virtual_slots, &mut symbols);
    let enums = module
        .items
        .iter()
        .filter_map(|item| {
            let hir::Item::Enum(declaration) = item else {
                return None;
            };
            Some(mir::EnumDefinition {
                symbol: declaration.symbol,
                name: declaration.name.clone(),
                cases: declaration
                    .cases
                    .iter()
                    .map(|case| mir::EnumCaseDefinition {
                        symbol: case.symbol,
                        name: case.name.clone(),
                        tag: case.tag,
                        fields: case
                            .fields
                            .iter()
                            .map(|field| mir::FieldDefinition {
                                symbol: field.symbol,
                                name: field.name.clone(),
                                type_: field.type_.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    let enum_cases = Arc::new(
        enums
            .iter()
            .flat_map(|value| value.cases.iter())
            .map(|case| (case.symbol, case.clone()))
            .collect::<HashMap<_, _>>(),
    );
    let mut functions = Vec::new();
    let mut foreign_functions = Vec::new();
    for item in &module.items {
        match item {
            hir::Item::Function(function) => {
                if function.is_foreign {
                    foreign_functions.push(mir::ForeignFunction {
                        symbol: function.symbol,
                        name: function.name.clone(),
                        parameters: function
                            .parameters
                            .iter()
                            .map(|parameter| parameter.type_.clone())
                            .collect(),
                        return_type: function.return_type.clone(),
                    });
                    continue;
                }
                push_function(
                    &mut functions,
                    function,
                    None,
                    &intrinsics,
                    &enum_cases,
                    &virtual_slots,
                    &mut symbols,
                );
            }
            hir::Item::Class(declaration) | hir::Item::Struct(declaration) => {
                for method in &declaration.methods {
                    push_function(
                        &mut functions,
                        method,
                        Some(declaration.symbol),
                        &intrinsics,
                        &enum_cases,
                        &virtual_slots,
                        &mut symbols,
                    );
                }
            }
            hir::Item::Interface(_) | hir::Item::Enum(_) | hir::Item::Variable(_) => {}
        }
    }
    functions.extend(thunks);
    order_struct_aggregates(&mut functions, &structs);
    mir::Module {
        structs,
        classes,
        interfaces,
        enums,
        interface_implementations,
        foreign_functions,
        functions,
    }
}

#[allow(clippy::too_many_lines)]
fn lower_interfaces(
    module: &hir::Module,
    vtables: &HashMap<hir::SymbolId, Vec<mir::VirtualSlot>>,
    virtual_slots: &HashMap<hir::SymbolId, (u32, hir::SymbolId)>,
    symbols: &mut SymbolAllocator,
) -> (
    Vec<mir::InterfaceDefinition>,
    Vec<mir::InterfaceImplementation>,
    Vec<mir::Function>,
) {
    let interfaces = module
        .items
        .iter()
        .filter_map(|item| {
            let hir::Item::Interface(declaration) = item else {
                return None;
            };
            Some(mir::InterfaceDefinition {
                symbol: declaration.symbol,
                name: declaration.name.clone(),
                methods: declaration
                    .methods
                    .iter()
                    .map(|method| mir::InterfaceMethodDefinition {
                        symbol: method.symbol,
                        name: method.name.clone(),
                        parameters: method
                            .parameters
                            .iter()
                            .map(|parameter| parameter.type_.clone())
                            .collect(),
                        return_type: method.return_type.clone(),
                    })
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    let interface_methods = interfaces
        .iter()
        .map(|interface| (interface.symbol, interface.methods.clone()))
        .collect::<HashMap<_, _>>();
    let classes = module
        .items
        .iter()
        .filter_map(|item| match item {
            hir::Item::Class(declaration) => Some((declaration.symbol, declaration)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut interface_implementations = Vec::new();
    // One dispatching function per (class, slot) whose interface entry must
    // run the most derived override.
    let mut thunks = Vec::new();
    let mut thunk_symbols: HashMap<(hir::SymbolId, hir::SymbolId), hir::SymbolId> = HashMap::new();
    for item in &module.items {
        let hir::Item::Class(declaration) = item else {
            continue;
        };
        // The class itself, then its base classes; the most derived
        // declaration of a method wins.
        let mut chain = vec![declaration];
        while let Some(base) = chain.last().and_then(|class| class.base) {
            chain.push(classes[&base]);
        }
        let mut seen = std::collections::HashSet::new();
        for interface in chain.iter().flat_map(|class| &class.interfaces) {
            if !seen.insert(*interface) {
                continue;
            }
            let methods = interface_methods[interface]
                .iter()
                .map(|required| {
                    let implementing = chain
                        .iter()
                        .find_map(|class| {
                            class.methods.iter().find(|method| {
                                method.name == required.name
                                    && method.return_type == required.return_type
                                    && method
                                        .parameters
                                        .iter()
                                        .skip(1)
                                        .map(|parameter| &parameter.type_)
                                        .eq(required.parameters.iter())
                            })
                        })
                        .expect("validated class implements every interface method");
                    let Some(&(slot, root)) = virtual_slots.get(&implementing.symbol) else {
                        return implementing.symbol;
                    };
                    // A virtual implementation. A sealed class has no
                    // subclass, so its own vtable entry is exact; any other
                    // class may be the static type of an object of a derived
                    // class that overrides the method, so the entry
                    // dispatches through the object's descriptor.
                    let table = &vtables[&declaration.symbol];
                    if declaration.is_sealed {
                        return table[slot as usize]
                            .implementation
                            .expect("a sealed class implements every virtual slot");
                    }
                    *thunk_symbols
                        .entry((declaration.symbol, root))
                        .or_insert_with(|| {
                            let symbol = symbols.fresh();
                            thunks.push(virtual_thunk(
                                symbol,
                                declaration,
                                slot,
                                &table[slot as usize],
                            ));
                            symbol
                        })
                })
                .collect();
            interface_implementations.push(mir::InterfaceImplementation {
                class: declaration.symbol,
                interface: *interface,
                methods,
            });
        }
    }
    (interfaces, interface_implementations, thunks)
}

/// Virtual table of every class.
type ClassTables = HashMap<hir::SymbolId, Vec<mir::VirtualSlot>>;
/// `(slot index, introducing method)` of every virtual, abstract or overriding method.
type VirtualSlots = HashMap<hir::SymbolId, (u32, hir::SymbolId)>;

/// The virtual table of every class and the slot of every virtual member.
///
/// A class's table starts with its base class's table (an inherited slot keeps
/// its index and introducing method); an `override` replaces the implementation
/// of the slot it overrides and reuses its index; `virtual` and `abstract`
/// members append new slots. The result maps each virtual, abstract or
/// overriding method symbol to `(slot, introducing method)`.
fn build_virtual_tables(module: &hir::Module) -> (ClassTables, VirtualSlots) {
    let classes = module
        .items
        .iter()
        .filter_map(|item| match item {
            hir::Item::Class(declaration) => Some((declaration.symbol, declaration)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut tables = HashMap::new();
    let mut slots = HashMap::new();
    for class in classes.keys().copied().collect::<Vec<_>>() {
        class_table(class, &classes, &mut tables, &mut slots);
    }
    (tables, slots)
}

fn class_table(
    class: hir::SymbolId,
    classes: &HashMap<hir::SymbolId, &hir::TypeDeclaration>,
    tables: &mut HashMap<hir::SymbolId, Vec<mir::VirtualSlot>>,
    slots: &mut HashMap<hir::SymbolId, (u32, hir::SymbolId)>,
) {
    if tables.contains_key(&class) {
        return;
    }
    let declaration = classes[&class];
    let mut table = declaration.base.map_or_else(Vec::new, |base| {
        class_table(base, classes, tables, slots);
        tables[&base].clone()
    });
    for method in &declaration.methods {
        match method.dispatch {
            hir::MethodDispatch::None => {}
            hir::MethodDispatch::Virtual | hir::MethodDispatch::Abstract => {
                slots.insert(
                    method.symbol,
                    (
                        u32::try_from(table.len()).expect("virtual slot count fits u32"),
                        method.symbol,
                    ),
                );
                table.push(mir::VirtualSlot {
                    method: method.symbol,
                    name: method.name.clone(),
                    parameters: method
                        .parameters
                        .iter()
                        .skip(1)
                        .map(|parameter| parameter.type_.clone())
                        .collect(),
                    return_type: method.return_type.clone(),
                    implementation: (method.dispatch == hir::MethodDispatch::Virtual)
                        .then_some(method.symbol),
                });
            }
            hir::MethodDispatch::Override(overridden) => {
                let (slot, root) = slots[&overridden];
                slots.insert(method.symbol, (slot, root));
                table[slot as usize].implementation = Some(method.symbol);
            }
        }
    }
    tables.insert(class, table);
}

/// `fn(this: Class, parameters...) { return this.<virtual slot>(parameters...) }`:
/// the interface-table entry of a class whose implementing method is virtual.
fn virtual_thunk(
    symbol: hir::SymbolId,
    class: &hir::TypeDeclaration,
    slot: u32,
    entry: &mir::VirtualSlot,
) -> mir::Function {
    let parameter = |index: u32, name: String, type_: mir::Type| mir::Local {
        id: mir::LocalId(index),
        symbol: None,
        name,
        type_,
        mutable: false,
        temporary: false,
    };
    let copy = |local: &mir::Local| mir::Operand {
        type_: local.type_.clone(),
        kind: mir::OperandKind::Copy(mir::Place::Local(local.id)),
    };
    let mut parameters = vec![parameter(
        0,
        "this".to_owned(),
        mir::Type::Class(class.symbol),
    )];
    for (index, type_) in entry.parameters.iter().enumerate() {
        parameters.push(parameter(
            u32::try_from(index + 1).expect("parameter count fits u32"),
            format!("p{index}"),
            type_.clone(),
        ));
    }
    let arguments = parameters.iter().skip(1).map(copy).collect();
    let (locals, destination, terminator) = if entry.return_type == mir::Type::Void {
        (Vec::new(), None, mir::Terminator::Return(None))
    } else {
        let result = mir::Local {
            id: mir::LocalId(u32::try_from(parameters.len()).expect("parameter count fits u32")),
            symbol: None,
            name: "_result".to_owned(),
            type_: entry.return_type.clone(),
            mutable: true,
            temporary: true,
        };
        let place = mir::Place::Local(result.id);
        let value = mir::Operand {
            type_: entry.return_type.clone(),
            kind: mir::OperandKind::Copy(place.clone()),
        };
        (
            vec![result],
            Some(place),
            mir::Terminator::Return(Some(value)),
        )
    };
    let receiver = copy(&parameters[0]);
    mir::Function {
        constructor: false,
        symbol,
        owner: Some(class.symbol),
        name: format!("{}.{}$virtual", class.name, entry.name),
        visibility: mir::Visibility::Public,
        parameters,
        locals,
        return_type: entry.return_type.clone(),
        entry: mir::BasicBlockId(0),
        blocks: vec![mir::BasicBlock {
            id: mir::BasicBlockId(0),
            instructions: vec![mir::Instruction::CallVirtual {
                destination,
                receiver,
                method: entry.method,
                slot,
                arguments,
                return_type: entry.return_type.clone(),
            }],
            terminator,
        }],
        temporary_subregion_candidates: Vec::new(),
    }
}

fn push_function(
    functions: &mut Vec<mir::Function>,
    function: &hir::Function,
    owner: Option<hir::SymbolId>,
    intrinsics: &HashMap<hir::SymbolId, hir::Intrinsic>,
    enum_cases: &Arc<HashMap<hir::SymbolId, mir::EnumCaseDefinition>>,
    virtual_slots: &Arc<HashMap<hir::SymbolId, (u32, hir::SymbolId)>>,
    symbols: &mut SymbolAllocator,
) {
    if function.body.is_none() || function.intrinsic.is_some() {
        return;
    }
    if function.is_async {
        // An async function becomes a wrapper (keeping its symbol) plus a
        // generated `MoveNext` with a fresh, unique symbol referenced only by
        // identity — never by name.
        let move_next_symbol = symbols.fresh();
        let (wrapper, move_next) = async_machine::lower(
            function,
            owner,
            intrinsics,
            enum_cases,
            virtual_slots,
            move_next_symbol,
        );
        functions.push(wrapper);
        functions.push(move_next);
    } else {
        functions.push(
            FunctionLowerer::new(function, intrinsics.clone(), Arc::clone(enum_cases))
                .with_virtual_slots(Arc::clone(virtual_slots))
                .lower(function, owner),
        );
    }
}

/// A struct literal's fields are lowered in source order so their
/// expressions keep left-to-right evaluation; MIR's `Aggregate` contract is
/// declaration order. The operands are already evaluated when the `Aggregate`
/// runs, so permuting them changes no evaluation order (the same rule enum
/// payloads follow).
fn order_struct_aggregates(functions: &mut [mir::Function], structs: &[mir::StructDefinition]) {
    let order = structs
        .iter()
        .map(|definition| {
            let positions = definition
                .fields
                .iter()
                .enumerate()
                .map(|(position, field)| (field.symbol, position))
                .collect::<HashMap<_, _>>();
            (definition.symbol, positions)
        })
        .collect::<HashMap<_, _>>();
    for function in functions {
        for block in &mut function.blocks {
            for instruction in &mut block.instructions {
                let mir::Instruction::Assign { value, .. } = instruction else {
                    continue;
                };
                let (mir::Type::User(symbol), mir::RvalueKind::Aggregate(fields)) =
                    (&value.type_, &mut value.kind)
                else {
                    continue;
                };
                if let Some(positions) = order.get(symbol) {
                    fields.sort_by_key(|field| positions.get(&field.field).copied());
                }
            }
        }
    }
}
