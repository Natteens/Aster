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
    let classes = module
        .items
        .iter()
        .filter_map(|item| {
            let hir::Item::Class(declaration) = item else {
                return None;
            };
            Some(mir::ClassDefinition {
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
        .collect();
    let (interfaces, interface_implementations) = lower_interfaces(module);
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
                        &mut symbols,
                    );
                }
            }
            hir::Item::Interface(_) | hir::Item::Enum(_) | hir::Item::Variable(_) => {}
        }
    }
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

fn lower_interfaces(
    module: &hir::Module,
) -> (
    Vec<mir::InterfaceDefinition>,
    Vec<mir::InterfaceImplementation>,
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
    let mut interface_implementations = Vec::new();
    for item in &module.items {
        let hir::Item::Class(declaration) = item else {
            continue;
        };
        for interface in &declaration.interfaces {
            let methods = interface_methods[interface]
                .iter()
                .map(|required| {
                    declaration
                        .methods
                        .iter()
                        .find(|method| {
                            method.name == required.name
                                && method.return_type == required.return_type
                                && method
                                    .parameters
                                    .iter()
                                    .skip(1)
                                    .map(|parameter| &parameter.type_)
                                    .eq(required.parameters.iter())
                        })
                        .expect("validated class implements every interface method")
                        .symbol
                })
                .collect();
            interface_implementations.push(mir::InterfaceImplementation {
                class: declaration.symbol,
                interface: *interface,
                methods,
            });
        }
    }
    (interfaces, interface_implementations)
}

fn push_function(
    functions: &mut Vec<mir::Function>,
    function: &hir::Function,
    owner: Option<hir::SymbolId>,
    intrinsics: &HashMap<hir::SymbolId, hir::Intrinsic>,
    enum_cases: &Arc<HashMap<hir::SymbolId, mir::EnumCaseDefinition>>,
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
        let (wrapper, move_next) =
            async_machine::lower(function, owner, intrinsics, enum_cases, move_next_symbol);
        functions.push(wrapper);
        functions.push(move_next);
    } else {
        functions.push(
            FunctionLowerer::new(function, intrinsics.clone(), Arc::clone(enum_cases))
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
