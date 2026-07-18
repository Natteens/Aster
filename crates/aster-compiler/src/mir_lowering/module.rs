use super::{FunctionLowerer, HashMap, hir, mir};

#[allow(clippy::too_many_lines)]
pub(super) fn lower(module: &hir::Module) -> mir::Module {
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
        .collect();
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
    let enum_map = enums
        .iter()
        .map(|value| (value.symbol, value.clone()))
        .collect::<HashMap<_, _>>();
    let mut functions = Vec::new();
    for item in &module.items {
        match item {
            hir::Item::Function(function) => {
                push_function(&mut functions, function, None, &intrinsics, &enum_map);
            }
            hir::Item::Class(declaration) | hir::Item::Struct(declaration) => {
                for method in &declaration.methods {
                    push_function(
                        &mut functions,
                        method,
                        Some(declaration.symbol),
                        &intrinsics,
                        &enum_map,
                    );
                }
            }
            hir::Item::Interface(_) | hir::Item::Enum(_) | hir::Item::Variable(_) => {}
        }
    }
    mir::Module {
        structs,
        classes,
        interfaces,
        enums,
        interface_implementations,
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
    enums: &HashMap<hir::SymbolId, mir::EnumDefinition>,
) {
    if function.body.is_some() && function.intrinsic.is_none() {
        functions.push(
            FunctionLowerer::new(function, intrinsics.clone(), enums.clone())
                .lower(function, owner),
        );
    }
}
