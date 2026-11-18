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
    /// Direct base class of every derived class, resolved from the first name
    /// after `:` when it names a class. Semantic analysis already proved the
    /// hierarchy acyclic.
    class_bases: HashMap<hir::SymbolId, hir::SymbolId>,
    interface_types: HashSet<hir::SymbolId>,
    enum_types: HashSet<hir::SymbolId>,
    enum_cases: HashMap<(String, usize), (hir::SymbolId, Vec<hir::SymbolId>)>,
    /// Same data as `enum_cases`, additionally indexed by declared case name.
    /// Populated alongside it (see `declarations::predeclare`'s enum arm) so
    /// M2D's filesystem intrinsics can resolve `Result<T, IOError>`'s `Ok`/
    /// `Error` cases and `IOErrorKind`'s named cases to concrete symbols once,
    /// during HIR lowering, instead of the backend comparing names.
    enum_case_by_name: HashMap<(String, String), (hir::SymbolId, Vec<hir::SymbolId>)>,
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
    string_builder: Option<StringBuilderSymbols>,
    option_meta: HashMap<hir::SymbolId, OptionMeta>,
    null_conditional_receivers: Vec<hir::SymbolId>,
}

#[derive(Clone)]
pub(super) struct OptionMeta {
    pub(super) some_case: hir::SymbolId,
    pub(super) some_tag: u32,
    pub(super) some_field: hir::SymbolId,
    pub(super) inner_type: hir::Type,
}

#[derive(Clone, Copy)]
struct StringBuilderSymbols {
    class: hir::SymbolId,
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
            class_bases: HashMap::new(),
            interface_types: HashSet::new(),
            enum_types: HashSet::new(),
            enum_cases: HashMap::new(),
            enum_case_by_name: HashMap::new(),
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
            string_builder: None,
            option_meta: HashMap::new(),
            null_conditional_receivers: Vec::new(),
        };
        lowerer.predeclare(module);
        lowerer.string_builder = lowerer
            .types
            .get(crate::standard_library::STRING_BUILDER_NAME)
            .map(|class| StringBuilderSymbols { class: *class });
        lowerer
    }

    pub(super) fn convert_to(
        &self,
        expression: hir::Expression,
        target: &hir::Type,
    ) -> hir::Expression {
        if &expression.type_ == target
            || expression.type_ == hir::Type::Unknown
            || *target == hir::Type::Unknown
        {
            return expression;
        }
        if let hir::Type::Enum(option_symbol) = target
            && let Some(meta) = self.option_meta.get(option_symbol)
        {
            let value = self.convert_to(expression, &meta.inner_type);
            return hir::Expression {
                type_: target.clone(),
                kind: hir::ExpressionKind::EnumValue {
                    enum_symbol: *option_symbol,
                    case: meta.some_case,
                    tag: meta.some_tag,
                    fields: vec![hir::FieldValue {
                        field: meta.some_field,
                        value,
                    }],
                },
            };
        }
        types::convert(expression, target)
    }

    pub(super) fn convert_expression(
        &mut self,
        expression: &ast::Expression,
        target: &hir::Type,
    ) -> hir::Expression {
        let value = self.expression(expression);
        self.convert_to(value, target)
    }

    pub(super) fn option_depth(&self, type_: &hir::Type) -> usize {
        let mut depth = 0;
        let mut current = type_;
        while let hir::Type::Enum(symbol) = current
            && let Some(meta) = self.option_meta.get(symbol)
        {
            depth += 1;
            current = &meta.inner_type;
        }
        depth
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
