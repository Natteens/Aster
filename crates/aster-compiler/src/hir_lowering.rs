mod declarations;
mod expressions;
mod statements;
mod types;

use std::collections::{HashMap, HashSet};

use aster_hir as hir;
use aster_syntax as ast;

use crate::constexpr::ConstValue;

pub(crate) fn lower(
    module: &ast::Module,
    model: &crate::semantic::Model,
    intrinsic_bindings: &HashMap<String, hir::Intrinsic>,
) -> hir::Module {
    Lowerer::new(module, model, intrinsic_bindings).module(module)
}

struct Lowerer<'a> {
    model: &'a crate::semantic::Model,
    intrinsic_bindings: &'a HashMap<String, hir::Intrinsic>,
    next_symbol: u32,
    globals: HashMap<String, hir::SymbolId>,
    scopes: Vec<HashMap<String, hir::SymbolId>>,
    types: HashMap<String, hir::SymbolId>,
    class_types: HashSet<hir::SymbolId>,
    interface_types: HashSet<hir::SymbolId>,
    enum_types: HashSet<hir::SymbolId>,
    enum_cases: HashMap<(String, usize), (hir::SymbolId, Vec<hir::SymbolId>)>,
    member_owners: HashMap<hir::SymbolId, hir::SymbolId>,
    current_receiver: Option<hir::SymbolId>,
    symbol_types: HashMap<hir::SymbolId, hir::Type>,
    callable_results: HashMap<hir::SymbolId, hir::Type>,
    callable_parameters: HashMap<hir::SymbolId, Vec<hir::Type>>,
    current_return: hir::Type,
    members: HashMap<hir::SymbolId, HashMap<String, hir::SymbolId>>,
    item_symbols: HashMap<String, hir::SymbolId>,
    member_symbols: HashMap<(String, String), hir::SymbolId>,
    callable_symbols: HashMap<crate::semantic::CallableKey, hir::SymbolId>,
    /// Evaluated values of `const` declarations, used to fold constant
    /// references into literals during lowering.
    constant_values: HashMap<hir::SymbolId, ConstValue>,
    model_context: String,
}

impl<'a> Lowerer<'a> {
    fn new(
        module: &ast::Module,
        model: &'a crate::semantic::Model,
        intrinsic_bindings: &'a HashMap<String, hir::Intrinsic>,
    ) -> Self {
        let mut lowerer = Self {
            model,
            intrinsic_bindings,
            next_symbol: 0,
            globals: HashMap::new(),
            scopes: Vec::new(),
            types: HashMap::new(),
            class_types: HashSet::new(),
            interface_types: HashSet::new(),
            enum_types: HashSet::new(),
            enum_cases: HashMap::new(),
            member_owners: HashMap::new(),
            current_receiver: None,
            symbol_types: HashMap::new(),
            callable_results: HashMap::new(),
            callable_parameters: HashMap::new(),
            current_return: hir::Type::Void,
            members: HashMap::new(),
            item_symbols: HashMap::new(),
            member_symbols: HashMap::new(),
            callable_symbols: HashMap::new(),
            constant_values: HashMap::new(),
            model_context: String::new(),
        };
        lowerer.predeclare(module);
        lowerer
    }

    fn module(mut self, module: &ast::Module) -> hir::Module {
        self.scopes.push(self.globals.clone());
        let mut items = Vec::new();
        for item in &module.items {
            match item {
                ast::Item::Class(item) => items.push(hir::Item::Class(self.type_declaration(item))),
                ast::Item::Struct(item) => {
                    items.push(hir::Item::Struct(self.type_declaration(item)));
                }
                ast::Item::Interface(item) => {
                    items.push(hir::Item::Interface(self.type_declaration(item)));
                }
                ast::Item::Enum(item) => {
                    items.push(hir::Item::Enum(self.enum_declaration(item)));
                }
                ast::Item::Function(item) => {
                    items.push(hir::Item::Function(self.function(item, None, None)));
                }
                ast::Item::Variable(item) => {
                    let symbol = self.item_symbols[&item.name];
                    items.push(hir::Item::Variable(self.variable(item, symbol)));
                }
            }
        }
        hir::Module { items }
    }

    fn lookup(&self, name: &str) -> Option<hir::SymbolId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    fn allocate(&mut self) -> hir::SymbolId {
        let symbol = hir::SymbolId(self.next_symbol);
        self.next_symbol += 1;
        symbol
    }
}
